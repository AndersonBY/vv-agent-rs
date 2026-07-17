use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use crate::app_server::durable_resume::{
    validate_completion, DurableTurnCompletionFuture, DurableTurnResumeOutcome,
    DurableTurnResumeProvider, DurableTurnResumeRequest,
};
use crate::app_server::host::{
    AgentResolutionRequest, AppServerHost, DefaultAppServerHost, RunConfigResolutionRequest,
};
use crate::app_server::outgoing::OutgoingMessageSender;
use crate::app_server::protocol::{
    map_run_event_to_notifications, AgentMessageDeltaParams, AppCacheUsage, AppItem,
    AppServerError, AppServerErrorCode, AppThread, AppTokenUsage, AppTurn, ApprovalDecision,
    ApprovalRequestParams, ApprovalResolveParams, CheckpointSummary, CheckpointSummaryStatus,
    InterruptionIdempotencySupport, InterruptionOperationKind, InterruptionSummary,
    ItemCompletedParams, ItemStartedParams, JsonRpcError, JsonRpcErrorBody, RequestId,
    ServerNotification, ServerRequest, ThreadStatus, ThreadStatusChangedParams,
    TurnCompletedParams, TurnResumeParams, TurnResumeResponse, TurnStartParams, TurnStartedParams,
    TurnStatus, UserInput,
};
use crate::app_server::thread_state::{ActiveTurn, SteeringQueue, ThreadStateManager};
use crate::app_server::thread_store::{ItemAppendOutcome, SqliteThreadStore, ThreadStoreError};
use crate::app_server::transport::ConnectionId;
use crate::checkpoint::{
    CheckpointStatus, OperationKind, ResumeObservation, ResumePolicy, ToolIdempotency,
    MAX_CHECKPOINT_KEY_BYTES,
};
use crate::events::RunEventPayload;
use crate::runner::CheckpointStartOutcome;
use crate::runtime::state_v2::{CheckpointStoreV2, CheckpointV2};
use crate::tools::ApprovalDecision as ToolApprovalDecision;
use crate::types::{AgentStatus, CacheUsageStatus, Metadata, TaskTokenUsage, ToolExecutionResult};
use crate::{
    Agent, ApprovalBroker, ApprovalFuture, ApprovalProvider, ApprovalRequest, BeforeLlmEvent,
    BeforeLlmPatch, BeforeToolCallEvent, BeforeToolCallPatch, Message, RunConfig, RunHandle,
    RunResult, Runner, RuntimeHook,
};

mod approval;
mod resume;

use approval::tool_approval_decision_from_response;
use resume::{checkpoint_projection, turn_completion_result};

#[derive(Clone)]
pub struct AppServerRunAdapter {
    runner: Runner,
    host: Arc<dyn AppServerHost>,
    store: SqliteThreadStore,
    state: ThreadStateManager,
    outgoing: OutgoingMessageSender,
    approval_request_timeout: Duration,
    turn_approval_timeouts: Arc<Mutex<HashMap<(String, String), Duration>>>,
    durable_resume_provider: Option<Arc<dyn DurableTurnResumeProvider>>,
}
pub(crate) struct PreparedTurnResume {
    response: TurnResumeResponse,
    continuation: Option<PreparedTurnResumeContinuation>,
}

struct PreparedTurnResumeContinuation {
    request: DurableTurnResumeRequest,
    completion: DurableTurnCompletionFuture,
}

impl PreparedTurnResume {
    pub(crate) fn response(&self) -> &TurnResumeResponse {
        &self.response
    }
}

impl AppServerRunAdapter {
    pub fn new(
        runner: Runner,
        agent: Agent,
        store: SqliteThreadStore,
        state: ThreadStateManager,
        outgoing: OutgoingMessageSender,
    ) -> Self {
        Self::with_host(
            runner,
            Arc::new(DefaultAppServerHost::from_agent(agent)),
            store,
            state,
            outgoing,
        )
    }

    pub fn with_host(
        runner: Runner,
        host: Arc<dyn AppServerHost>,
        store: SqliteThreadStore,
        state: ThreadStateManager,
        outgoing: OutgoingMessageSender,
    ) -> Self {
        Self {
            runner,
            host,
            store,
            state,
            outgoing,
            approval_request_timeout: Duration::from_secs(30),
            turn_approval_timeouts: Arc::new(Mutex::new(HashMap::new())),
            durable_resume_provider: None,
        }
    }

    pub fn with_approval_request_timeout(mut self, timeout: Duration) -> Self {
        self.approval_request_timeout = timeout;
        self
    }

    pub fn with_durable_resume_provider(
        mut self,
        provider: Arc<dyn DurableTurnResumeProvider>,
    ) -> Self {
        self.durable_resume_provider = Some(provider);
        self
    }

    pub fn store(&self) -> &SqliteThreadStore {
        &self.store
    }

    pub fn state(&self) -> &ThreadStateManager {
        &self.state
    }

    pub async fn start_turn(
        &self,
        owner_connection_id: ConnectionId,
        params: TurnStartParams,
    ) -> Result<AppTurn, AppServerError> {
        let thread = self
            .store
            .get_thread(&params.thread_id)
            .map_err(store_error)?
            .ok_or_else(AppServerError::thread_not_found)?;
        if thread.archived_at.is_some() {
            return Err(AppServerError::thread_archived());
        }
        if thread.status == ThreadStatus::Running
            || self.state.has_active_turn(&params.thread_id).await
        {
            return Err(AppServerError::invalid_params(
                "Thread already has an active turn",
            ));
        }

        let mut effective_metadata = thread.metadata.clone();
        effective_metadata.extend(params.metadata.clone());
        let agent_request = AgentResolutionRequest {
            thread_id: thread.thread_id.clone(),
            agent_key: thread.agent_key.clone(),
            cwd: thread.cwd.clone(),
            metadata: effective_metadata.clone(),
        };
        let config_request = RunConfigResolutionRequest {
            thread_id: thread.thread_id.clone(),
            agent_key: thread.agent_key.clone(),
            cwd: thread.cwd.clone(),
            metadata: effective_metadata.clone(),
        };
        let agent = self
            .host
            .resolve_agent(&agent_request)
            .map_err(|error| AppServerError::internal(error.to_string()))?;
        let mut config = self
            .host
            .build_run_config(&config_request)
            .map_err(|error| AppServerError::internal(error.to_string()))?;

        let turn = self
            .store
            .create_turn(&params.thread_id, params.input.clone())
            .map_err(store_error)?;
        let steering = SteeringQueue::default();
        if config.approval_provider.is_none() {
            config.approval_provider = Some(Arc::new(AppServerApprovalProvider));
        }
        if config.approval_broker.is_none() {
            config.approval_broker = Some(ApprovalBroker::default());
        }
        let approval_request_timeout =
            effective_approval_request_timeout(&config, self.approval_request_timeout);
        config.hooks.push(Arc::new(SteeringRuntimeHook {
            queue: steering.clone(),
        }));
        config.metadata.extend(effective_metadata);
        config
            .metadata
            .insert("thread_id".to_string(), json!(turn.thread_id));
        config
            .metadata
            .insert("turn_id".to_string(), json!(turn.turn_id));
        config
            .metadata
            .insert("session_id".to_string(), json!(turn.thread_id));
        let checkpoint_store = config
            .checkpoint_config
            .as_ref()
            .and_then(|checkpoint| checkpoint.store.clone());

        let handle = self
            .runner
            .start(&agent, input_text(&turn.input), config)
            .await
            .map_err(AppServerError::internal)?;
        self.store
            .set_active_turn(&turn.thread_id, Some(&turn.turn_id), ThreadStatus::Running)
            .map_err(store_error)?;
        self.turn_approval_timeouts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                (turn.thread_id.clone(), turn.turn_id.clone()),
                approval_request_timeout,
            );
        self.state
            .set_active_turn(
                turn.thread_id.clone(),
                ActiveTurn {
                    turn: turn.clone(),
                    handle,
                    steering,
                    owner_connection_id,
                    checkpoint_store,
                },
            )
            .await;
        Ok(turn)
    }

    pub async fn notify_turn_started(&self, turn: &AppTurn) -> Result<(), AppServerError> {
        self.broadcast_to_thread(
            &turn.thread_id,
            ServerNotification::ThreadStatusChanged(ThreadStatusChangedParams {
                thread_id: turn.thread_id.clone(),
                status: ThreadStatus::Running,
            }),
        )
        .await?;
        self.broadcast_to_thread(
            &turn.thread_id,
            ServerNotification::TurnStarted(TurnStartedParams {
                thread_id: turn.thread_id.clone(),
                turn_id: turn.turn_id.clone(),
                run_id: None,
                status: None,
            }),
        )
        .await
    }

    pub async fn spawn_event_forwarding(&self, thread_id: String, turn_id: String) {
        let Some(active) = self.state.active_turn(&thread_id).await else {
            return;
        };
        self.spawn_event_forwarding_for(active, thread_id, turn_id);
    }

    fn spawn_event_forwarding_for(&self, active: ActiveTurn, thread_id: String, turn_id: String) {
        let adapter = self.clone();
        tokio::spawn(async move {
            let mut events = active.handle.events();
            let mut tool_arguments = HashMap::<String, Value>::new();
            while let Some(event) = events.next().await {
                match event {
                    Ok(event) => {
                        if let RunEventPayload::ToolCallStarted {
                            tool_call_id,
                            arguments,
                            ..
                        } = event.payload()
                        {
                            tool_arguments.insert(tool_call_id.clone(), arguments.clone());
                        }
                        let mut notifications =
                            map_run_event_to_notifications(&thread_id, &turn_id, &event);
                        for notification in &mut notifications {
                            if let ServerNotification::ApprovalRequested(approval) = notification {
                                if let Some(arguments) = tool_arguments.get(&approval.tool_call_id)
                                {
                                    approval.arguments = arguments.clone();
                                }
                            }
                        }
                        if let Some(item) = notifications.iter().find_map(item_from_notification) {
                            match adapter.store.append_item(&thread_id, &turn_id, item) {
                                Ok(ItemAppendOutcome::Inserted) => {}
                                Ok(ItemAppendOutcome::AlreadyPresent) => continue,
                                Err(error) => {
                                    let _ = adapter
                                        .broadcast_to_thread(
                                            &thread_id,
                                            ServerNotification::ErrorWarning(
                                                crate::app_server::protocol::WarningParams {
                                                    message: error.to_string(),
                                                    code: Some(
                                                        "item_identity_conflict".to_string(),
                                                    ),
                                                },
                                            ),
                                        )
                                        .await;
                                    continue;
                                }
                            }
                        }
                        for notification in notifications {
                            // The route owns the canonical decision. The runtime event currently
                            // carries only `approved`, so forwarding it would collapse
                            // `allow_session` to `allow` and emit a duplicate resolution.
                            if matches!(notification, ServerNotification::ApprovalResolved(_)) {
                                continue;
                            }
                            let _ = adapter
                                .broadcast_to_thread(&thread_id, notification.clone())
                                .await;
                            if let ServerNotification::ApprovalRequested(approval) = notification {
                                adapter
                                    .route_approval_request(
                                        &thread_id,
                                        &turn_id,
                                        active.owner_connection_id,
                                        &active.handle,
                                        approval,
                                    )
                                    .await;
                            }
                        }
                    }
                    Err(error) => {
                        let _ = adapter
                            .broadcast_to_thread(
                                &thread_id,
                                ServerNotification::ErrorWarning(
                                    crate::app_server::protocol::WarningParams {
                                        message: error,
                                        code: Some("event_stream".to_string()),
                                    },
                                ),
                            )
                            .await;
                    }
                }
            }
            let result = events.into_result().await;
            adapter
                .complete_turn(
                    thread_id,
                    turn_id,
                    active.owner_connection_id,
                    active.checkpoint_store,
                    result,
                )
                .await;
        });
    }

    pub async fn queue_steering(
        &self,
        thread_id: &str,
        expected_turn_id: &str,
        input: Vec<UserInput>,
    ) -> Result<String, AppServerError> {
        let active = self
            .validated_active_turn(thread_id, expected_turn_id)
            .await?;
        active
            .steering
            .lock()
            .map_err(|_| AppServerError::internal("steering queue lock poisoned"))?
            .push_back(input);
        Ok(active.turn.turn_id)
    }

    pub async fn queue_follow_up(
        &self,
        thread_id: &str,
        expected_turn_id: &str,
        input: Vec<UserInput>,
    ) -> Result<String, AppServerError> {
        let active = self
            .validated_active_turn(thread_id, expected_turn_id)
            .await?;
        self.state.queue_follow_up(thread_id, input).await;
        Ok(active.turn.turn_id)
    }

    pub async fn interrupt_turn(
        &self,
        thread_id: &str,
        expected_turn_id: &str,
    ) -> Result<InterruptTurnOutcome, AppServerError> {
        let active = self
            .validated_active_turn(thread_id, expected_turn_id)
            .await?;
        let pending = self.state.pending_approval(thread_id).await;
        active.handle.cancel();
        let mut approval_resolved = None;
        if let Some(pending) = pending.filter(|pending| pending.turn_id == active.turn.turn_id) {
            let _ = active
                .handle
                .approve(
                    &pending.request_id,
                    ToolApprovalDecision::timeout("turn interrupted"),
                )
                .await;
            let _ = self
                .outgoing
                .resolve_server_error(
                    pending.connection_id,
                    JsonRpcError {
                        id: RequestId::String(pending.request_id.clone()),
                        error: JsonRpcErrorBody {
                            code: AppServerErrorCode::InternalError.code(),
                            message: "turn interrupted".to_string(),
                            data: None,
                        },
                    },
                )
                .await;
            self.state
                .clear_pending_approval(thread_id, &pending.request_id)
                .await;
            approval_resolved = Some(ApprovalResolveParams {
                thread_id: thread_id.to_string(),
                turn_id: active.turn.turn_id.clone(),
                request_id: pending.request_id,
                decision: ApprovalDecision::Timeout,
                reason: "turn interrupted".to_string(),
                metadata: Metadata::new(),
            });
        }
        Ok(InterruptTurnOutcome {
            approval_resolved,
            turn_id: active.turn.turn_id,
            cancelled: true,
        })
    }

    pub async fn notify_approval_resolved(
        &self,
        params: ApprovalResolveParams,
    ) -> Result<(), AppServerError> {
        let thread_id = params.thread_id.clone();
        self.broadcast_to_thread(&thread_id, ServerNotification::ApprovalResolved(params))
            .await
    }

    pub async fn active_turn(&self, thread_id: &str) -> Option<AppTurn> {
        self.state
            .active_turn(thread_id)
            .await
            .map(|active| active.turn)
    }

    async fn validated_active_turn(
        &self,
        thread_id: &str,
        expected_turn_id: &str,
    ) -> Result<ActiveTurn, AppServerError> {
        let active = self
            .state
            .active_turn(thread_id)
            .await
            .ok_or_else(AppServerError::active_turn_not_found)?;
        if !expected_turn_id.is_empty() && active.turn.turn_id != expected_turn_id {
            return Err(AppServerError::turn_id_mismatch());
        }
        Ok(active)
    }

    fn persist_turn_completion(
        &self,
        completion: &TurnCompletedParams,
    ) -> Result<AppTurn, AppServerError> {
        self.store
            .update_turn(
                &completion.turn_id,
                completion.status,
                completion.run_id.as_deref(),
                &turn_completion_result(completion),
            )
            .map_err(store_error)
    }

    async fn complete_turn(
        &self,
        thread_id: String,
        turn_id: String,
        owner_connection_id: ConnectionId,
        checkpoint_store: Option<Arc<dyn CheckpointStoreV2>>,
        result: Result<RunResult, String>,
    ) {
        let (
            status,
            run_id,
            final_output,
            completion_reason,
            completion_tool_name,
            partial_output,
            error,
            token_usage,
            budget_usage,
            budget_exhaustion,
            checkpoint,
            interruption,
        ) = match result {
            Ok(result) => {
                let status = turn_status(result.status());
                let error = if status == TurnStatus::Failed {
                    result
                        .result()
                        .error
                        .clone()
                        .or_else(|| result.result().wait_reason.clone())
                        .or_else(|| Some("Turn failed".to_string()))
                } else {
                    None
                };
                let (checkpoint, interruption) =
                    checkpoint_projection(&result, checkpoint_store.as_ref()).unwrap_or_else(
                        |error| {
                            eprintln!(
                        "warning: App Server checkpoint projection failed for turn {}: {error}",
                        turn_id
                    );
                            (None, None)
                        },
                    );
                (
                    status,
                    Some(result.run_id().to_string()),
                    result.final_output().map(str::to_string),
                    result
                        .completion_reason()
                        .map(|reason| reason.as_str().to_string()),
                    result.completion_tool_name().map(str::to_string),
                    result.partial_output().map(str::to_string),
                    error,
                    Some(app_token_usage(&result.result().token_usage)),
                    result.budget_usage().map(app_json_object),
                    result.budget_exhaustion().map(app_json_object),
                    checkpoint,
                    interruption,
                )
            }
            Err(error) => (
                TurnStatus::Failed,
                None,
                None,
                Some("failed".to_string()),
                None,
                None,
                Some(error),
                None,
                None,
                None,
                None,
                None,
            ),
        };
        let mut stored_result = std::collections::BTreeMap::new();
        if let Some(final_output) = &final_output {
            stored_result.insert("finalOutput".to_string(), json!(final_output));
        }
        if let Some(completion_reason) = &completion_reason {
            stored_result.insert("completionReason".to_string(), json!(completion_reason));
        }
        if let Some(completion_tool_name) = &completion_tool_name {
            stored_result.insert(
                "completionToolName".to_string(),
                json!(completion_tool_name),
            );
        }
        if let Some(partial_output) = &partial_output {
            stored_result.insert("partialOutput".to_string(), json!(partial_output));
        }
        if let Some(error) = &error {
            stored_result.insert("error".to_string(), json!(error));
        }
        if let Some(token_usage) = &token_usage {
            stored_result.insert("tokenUsage".to_string(), json!(token_usage));
        }
        if let Some(budget_usage) = &budget_usage {
            stored_result.insert("budgetUsage".to_string(), json!(budget_usage));
        }
        if let Some(budget_exhaustion) = &budget_exhaustion {
            stored_result.insert("budgetExhaustion".to_string(), json!(budget_exhaustion));
        }
        if let Some(checkpoint) = &checkpoint {
            stored_result.insert("checkpoint".to_string(), json!(checkpoint));
        }
        if let Some(interruption) = &interruption {
            stored_result.insert("interruption".to_string(), json!(interruption));
        }
        let _ = self
            .store
            .update_turn(&turn_id, status, run_id.as_deref(), &stored_result);
        self.turn_approval_timeouts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&(thread_id.clone(), turn_id.clone()));
        self.state.clear_active_turn(&thread_id, &turn_id).await;
        let _ = self
            .store
            .set_active_turn(&thread_id, None, ThreadStatus::Idle);
        let _ = self
            .broadcast_to_thread(
                &thread_id,
                ServerNotification::ThreadStatusChanged(ThreadStatusChangedParams {
                    thread_id: thread_id.clone(),
                    status: ThreadStatus::Idle,
                }),
            )
            .await;
        let _ = self
            .broadcast_to_thread(
                &thread_id,
                ServerNotification::TurnCompleted(TurnCompletedParams {
                    thread_id: thread_id.clone(),
                    turn_id: turn_id.clone(),
                    run_id,
                    status,
                    final_output,
                    completion_reason,
                    completion_tool_name,
                    partial_output,
                    error,
                    token_usage,
                    budget_usage,
                    budget_exhaustion,
                    checkpoint,
                    interruption,
                }),
            )
            .await;

        if status == TurnStatus::Completed {
            if let Some(input) = self.state.pop_follow_up(&thread_id).await {
                let params = TurnStartParams {
                    thread_id: thread_id.clone(),
                    input,
                    metadata: Default::default(),
                };
                match self.start_turn(owner_connection_id, params).await {
                    Ok(turn) => {
                        let _ = self.notify_turn_started(&turn).await;
                        if let Some(active) = self.state.active_turn(&thread_id).await {
                            self.spawn_event_forwarding_for(active, thread_id, turn.turn_id);
                        }
                    }
                    Err(error) => {
                        let _ = self
                            .broadcast_to_thread(
                                &thread_id,
                                ServerNotification::ErrorWarning(
                                    crate::app_server::protocol::WarningParams {
                                        message: error.message().to_string(),
                                        code: Some("follow_up".to_string()),
                                    },
                                ),
                            )
                            .await;
                    }
                }
            }
        }
    }

    async fn broadcast_to_thread(
        &self,
        thread_id: &str,
        notification: ServerNotification,
    ) -> Result<(), AppServerError> {
        let subscribers = self.state.subscribers(thread_id).await;
        for connection_id in subscribers {
            self.outgoing
                .send_notification(connection_id, notification.clone())
                .await?;
        }
        Ok(())
    }

    async fn route_approval_request(
        &self,
        thread_id: &str,
        turn_id: &str,
        owner_connection_id: ConnectionId,
        handle: &RunHandle,
        approval: ApprovalRequestParams,
    ) {
        if !self
            .state
            .is_subscribed(thread_id, owner_connection_id)
            .await
        {
            let reason = "approval client disconnected";
            let _ = handle
                .approve(&approval.request_id, ToolApprovalDecision::timeout(reason))
                .await;
            let _ = self
                .broadcast_to_thread(
                    thread_id,
                    ServerNotification::ApprovalResolved(ApprovalResolveParams {
                        thread_id: thread_id.to_string(),
                        turn_id: turn_id.to_string(),
                        request_id: approval.request_id,
                        decision: ApprovalDecision::Timeout,
                        reason: reason.to_string(),
                        metadata: Metadata::new(),
                    }),
                )
                .await;
            return;
        }
        self.state
            .set_pending_approval(
                thread_id,
                turn_id,
                approval.request_id.clone(),
                owner_connection_id,
            )
            .await;
        let timeout = self
            .turn_approval_timeouts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(thread_id.to_string(), turn_id.to_string()))
            .copied()
            .unwrap_or(self.approval_request_timeout);
        let response = self
            .outgoing
            .send_server_request_with_id_and_timeout(
                owner_connection_id,
                RequestId::String(approval.request_id.clone()),
                ServerRequest::ApprovalRequest(approval.clone()),
                timeout,
            )
            .await;
        let (decision, protocol_decision) = match response {
            Ok(value) => tool_approval_decision_from_response(value),
            Err(error) => (
                ToolApprovalDecision::timeout(error.message().to_string()),
                ApprovalDecision::Timeout,
            ),
        };
        let resolution_reason = decision.reason().to_string();
        let resolution_metadata = decision.metadata().cloned().unwrap_or_default();
        if self
            .state
            .pending_approval(thread_id)
            .await
            .is_none_or(|pending| pending.request_id != approval.request_id)
        {
            return;
        }
        self.state
            .clear_pending_approval(thread_id, &approval.request_id)
            .await;
        let _ = handle.approve(&approval.request_id, decision).await;
        let _ = self
            .broadcast_to_thread(
                thread_id,
                ServerNotification::ApprovalResolved(ApprovalResolveParams {
                    thread_id: thread_id.to_string(),
                    turn_id: turn_id.to_string(),
                    request_id: approval.request_id,
                    decision: protocol_decision,
                    reason: resolution_reason,
                    metadata: resolution_metadata,
                }),
            )
            .await;
    }
}

pub struct InterruptTurnOutcome {
    pub approval_resolved: Option<ApprovalResolveParams>,
    pub turn_id: String,
    pub cancelled: bool,
}

struct AppServerApprovalProvider;

impl ApprovalProvider for AppServerApprovalProvider {
    fn should_request(&self, _request: &ApprovalRequest) -> bool {
        true
    }

    fn decide(&self, _request: &ApprovalRequest) -> ApprovalFuture<Option<ToolApprovalDecision>> {
        Box::pin(async { Ok(None) })
    }
}

struct SteeringRuntimeHook {
    queue: SteeringQueue,
}

impl RuntimeHook for SteeringRuntimeHook {
    fn before_llm(&self, event: BeforeLlmEvent<'_>) -> Option<BeforeLlmPatch> {
        let queued = {
            let Ok(mut queue) = self.queue.lock() else {
                return None;
            };
            queue.drain(..).collect::<Vec<_>>()
        };
        if queued.is_empty() {
            return None;
        }
        let mut messages = event.messages.to_vec();
        messages.extend(
            queued
                .iter()
                .map(|input| Message::user(input_text(input)))
                .filter(|message| !message.content.is_empty()),
        );
        Some(BeforeLlmPatch {
            messages: Some(messages),
            tool_schemas: None,
        })
    }

    fn before_tool_call(&self, event: BeforeToolCallEvent<'_>) -> Option<BeforeToolCallPatch> {
        let has_steering = self
            .queue
            .lock()
            .map(|queue| !queue.is_empty())
            .unwrap_or(false);
        has_steering.then(|| {
            ToolExecutionResult::success(
                event.call.id.clone(),
                "Tool skipped due to queued steering message.",
            )
            .into()
        })
    }
}

fn effective_approval_request_timeout(config: &RunConfig, fallback: Duration) -> Duration {
    config.approval_timeout.unwrap_or(fallback)
}

fn item_from_notification(notification: &ServerNotification) -> Option<AppItem> {
    match notification {
        ServerNotification::AgentMessageDelta(AgentMessageDeltaParams { item, .. })
        | ServerNotification::ItemStarted(ItemStartedParams { item })
        | ServerNotification::ItemCompleted(ItemCompletedParams { item }) => Some(item.clone()),
        _ => None,
    }
}

fn input_text(input: &[UserInput]) -> String {
    input
        .iter()
        .filter_map(|item| {
            if item.get("type").and_then(Value::as_str) == Some("text") {
                item.get("text").and_then(Value::as_str).map(str::to_string)
            } else if let Some(text) = item.get("text").and_then(Value::as_str) {
                Some(text.to_string())
            } else if item.is_null() {
                None
            } else {
                Some(item.to_string())
            }
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn turn_status(status: AgentStatus) -> TurnStatus {
    match status {
        AgentStatus::WaitUser | AgentStatus::ReconciliationRequired => TurnStatus::Interrupted,
        AgentStatus::Completed => TurnStatus::Completed,
        AgentStatus::Pending | AgentStatus::Running => TurnStatus::Running,
        AgentStatus::Failed | AgentStatus::MaxCycles => TurnStatus::Failed,
    }
}

fn app_token_usage(usage: &TaskTokenUsage) -> AppTokenUsage {
    AppTokenUsage {
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
        total_tokens: usage.total_tokens,
        cached_tokens: usage.cached_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_creation_tokens: usage.cache_creation_tokens,
        cache_usage: AppCacheUsage {
            status: match usage.cache_usage.status {
                CacheUsageStatus::ProviderReported => "provider_reported",
                CacheUsageStatus::AccountingMissing => "accounting_missing",
                CacheUsageStatus::Unsupported => "unsupported",
            }
            .to_string(),
            read_tokens: usage.cache_usage.read_tokens,
            write_tokens: usage.cache_usage.write_tokens,
            uncached_input_tokens: usage.cache_usage.uncached_input_tokens,
            source: usage.cache_usage.source.clone(),
        },
    }
}

fn app_json_object(value: &impl serde::Serialize) -> BTreeMap<String, Value> {
    let Value::Object(fields) =
        serde_json::to_value(value).expect("typed App Server observation must serialize")
    else {
        unreachable!("typed App Server observation must serialize as an object");
    };
    fields.into_iter().collect()
}

fn store_error(error: ThreadStoreError) -> AppServerError {
    AppServerError::internal(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_run_config_approval_timeout_overrides_adapter_fallback() {
        let configured = Duration::from_millis(25);
        let config = RunConfig {
            approval_timeout: Some(configured),
            ..RunConfig::default()
        };

        assert_eq!(
            effective_approval_request_timeout(&config, Duration::from_secs(30)),
            configured
        );
        assert_eq!(
            effective_approval_request_timeout(&RunConfig::default(), Duration::from_secs(30)),
            Duration::from_secs(30)
        );
    }
}
