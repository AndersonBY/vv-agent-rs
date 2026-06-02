use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::app_server::outgoing::OutgoingMessageSender;
use crate::app_server::protocol::{
    map_run_event_to_notifications, AgentMessageDeltaParams, AppItem, AppItemKind, AppItemStatus,
    AppServerError, AppServerErrorCode, AppTurn, ApprovalDecision, ApprovalRequestParams,
    ApprovalResolveParams, ItemCompletedParams, ItemStartedParams, JsonRpcError, JsonRpcErrorBody,
    RequestId, ServerNotification, ServerRequest, ThreadStatus, TurnCompletedParams,
    TurnStartParams, TurnStartedParams, TurnStatus, UserInput,
};
use crate::app_server::thread_state::{ActiveTurn, ThreadStateManager};
use crate::app_server::thread_store::{SqliteThreadStore, ThreadStoreError};
use crate::tools::ApprovalDecision as ToolApprovalDecision;
use crate::{
    Agent, ApprovalBroker, ApprovalFuture, ApprovalProvider, ApprovalRequest, ModelRef, RunConfig,
    RunHandle, Runner,
};

#[derive(Clone)]
pub struct AppServerRunAdapter {
    runner: Runner,
    agent: Agent,
    store: SqliteThreadStore,
    state: ThreadStateManager,
    outgoing: OutgoingMessageSender,
    next_turn_id: Arc<AtomicU64>,
    approval_request_timeout: Duration,
}

impl AppServerRunAdapter {
    pub fn new(
        runner: Runner,
        agent: Agent,
        store: SqliteThreadStore,
        state: ThreadStateManager,
        outgoing: OutgoingMessageSender,
    ) -> Self {
        Self {
            runner,
            agent,
            store,
            state,
            outgoing,
            next_turn_id: Arc::new(AtomicU64::new(1)),
            approval_request_timeout: Duration::from_secs(30),
        }
    }

    pub fn with_approval_request_timeout(mut self, timeout: Duration) -> Self {
        self.approval_request_timeout = timeout;
        self
    }

    pub fn store(&self) -> &SqliteThreadStore {
        &self.store
    }

    pub fn state(&self) -> &ThreadStateManager {
        &self.state
    }

    pub async fn start_turn(&self, params: TurnStartParams) -> Result<AppTurn, AppServerError> {
        let thread = self
            .store
            .get_thread(&params.thread_id)
            .map_err(store_error)?
            .ok_or_else(|| AppServerError::invalid_params("Unknown thread"))?;
        if thread.archived {
            return Err(AppServerError::invalid_params("Thread is archived"));
        }
        if self.state.active_turn(&params.thread_id).await.is_some() {
            return Err(AppServerError::invalid_params(
                "Thread already has an active turn",
            ));
        }

        let turn_id = format!("turn_{}", self.next_turn_id.fetch_add(1, Ordering::Relaxed));
        let input = params.input.clone();
        let input_text = input_text(&input);
        let run_id = format!("{}_run", self.agent.name());
        let mut config = RunConfig::default();
        if let Some(model) = params.model {
            config.model = Some(ModelRef::named(model));
        }
        let approval_broker = ApprovalBroker::default();
        config.approval_provider = Some(Arc::new(AppServerApprovalProvider));
        config.approval_broker = Some(approval_broker);
        config
            .metadata
            .insert("thread_id".to_string(), json!(params.thread_id));
        config
            .metadata
            .insert("turn_id".to_string(), json!(turn_id.clone()));

        let handle = self
            .runner
            .start(&self.agent, input_text, config)
            .await
            .map_err(AppServerError::internal)?;
        let turn = AppTurn {
            id: turn_id.clone(),
            thread_id: params.thread_id.clone(),
            run_id,
            status: TurnStatus::Running,
            input,
            started_at_ms: Some(timestamp_millis()),
            completed_at_ms: None,
            token_usage: None,
        };
        self.store
            .set_active_turn(&params.thread_id, Some(&turn_id), ThreadStatus::Running)
            .map_err(store_error)?;
        self.state
            .set_active_turn(
                params.thread_id,
                ActiveTurn {
                    turn: turn.clone(),
                    handle,
                },
            )
            .await;
        Ok(turn)
    }

    pub async fn notify_turn_started(&self, turn: &AppTurn) -> Result<(), AppServerError> {
        self.broadcast_to_thread(
            &turn.thread_id,
            ServerNotification::TurnStarted(TurnStartedParams { turn: turn.clone() }),
        )
        .await
    }

    pub async fn spawn_event_forwarding(&self, thread_id: String, turn_id: String) {
        let Some(active) = self.state.active_turn(&thread_id).await else {
            return;
        };
        let adapter = self.clone();
        tokio::spawn(async move {
            let mut events = active.handle.events();
            while let Some(event) = events.next().await {
                match event {
                    Ok(event) => {
                        let notifications =
                            map_run_event_to_notifications(&thread_id, &turn_id, &event);
                        for notification in notifications {
                            if let Some(item) = item_from_notification(&notification) {
                                let _ = adapter.store.append_item(&thread_id, &turn_id, item);
                            }
                            let _ = adapter
                                .broadcast_to_thread(&thread_id, notification.clone())
                                .await;
                            if let ServerNotification::ApprovalRequested(approval) = &notification {
                                adapter
                                    .route_approval_request(
                                        &thread_id,
                                        &turn_id,
                                        &active.handle,
                                        approval.clone(),
                                    )
                                    .await;
                            }
                            if is_terminal_turn_notification(&notification) {
                                adapter.state.clear_active_turn(&thread_id, &turn_id).await;
                                let _ = adapter.store.set_active_turn(
                                    &thread_id,
                                    None,
                                    ThreadStatus::Idle,
                                );
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
                        break;
                    }
                }
            }
        });
    }

    pub async fn interrupt_turn(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<InterruptTurnOutcome, AppServerError> {
        if let Some(pending) = self.state.pending_approval(thread_id).await {
            if pending.turn_id == turn_id {
                let mut completed_turn = None;
                if let Some(active) = self.state.active_turn(thread_id).await {
                    active.handle.cancel();
                    let _ = active
                        .handle
                        .approve(
                            &pending.request_id,
                            ToolApprovalDecision::timeout("turn interrupted"),
                        )
                        .await;
                    completed_turn = Some(interrupted_turn(active.turn));
                }
                let _ = self
                    .outgoing
                    .resolve_server_error(JsonRpcError {
                        id: RequestId::String(pending.request_id.clone()),
                        error: JsonRpcErrorBody {
                            code: AppServerErrorCode::InternalError.code(),
                            message: "turn interrupted".to_string(),
                            data: None,
                        },
                    })
                    .await;
                self.state
                    .clear_pending_approval(thread_id, &pending.request_id)
                    .await;
                self.store
                    .set_active_turn(thread_id, None, ThreadStatus::Idle)
                    .map_err(store_error)?;
                return Ok(InterruptTurnOutcome {
                    approval_resolved: Some(ApprovalResolveParams {
                        thread_id: thread_id.to_string(),
                        turn_id: turn_id.to_string(),
                        request_id: pending.request_id,
                        decision: ApprovalDecision::Deny,
                    }),
                    completed_turn,
                });
            }
        }
        if let Some(active) = self.state.active_turn(thread_id).await {
            if active.turn.id != turn_id {
                return Err(AppServerError::invalid_params("No matching active turn"));
            }
            active.handle.cancel();
            self.store
                .set_active_turn(thread_id, None, ThreadStatus::Idle)
                .map_err(store_error)?;
            return Ok(InterruptTurnOutcome {
                approval_resolved: None,
                completed_turn: Some(interrupted_turn(active.turn)),
            });
        }
        Err(AppServerError::invalid_params("No matching active turn"))
    }

    pub async fn notify_approval_resolved(
        &self,
        params: ApprovalResolveParams,
    ) -> Result<(), AppServerError> {
        let thread_id = params.thread_id.clone();
        self.broadcast_to_thread(&thread_id, ServerNotification::ApprovalResolved(params))
            .await
    }

    pub async fn notify_turn_completed(&self, turn: AppTurn) -> Result<(), AppServerError> {
        let thread_id = turn.thread_id.clone();
        self.broadcast_to_thread(
            &thread_id,
            ServerNotification::TurnCompleted(TurnCompletedParams { turn }),
        )
        .await
    }

    pub async fn active_turn(&self, thread_id: &str) -> Option<AppTurn> {
        self.state
            .active_turn(thread_id)
            .await
            .map(|active| active.turn)
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
        handle: &RunHandle,
        approval: ApprovalRequestParams,
    ) {
        let subscribers = self.state.subscribers(thread_id).await;
        let Some(connection_id) = subscribers.first().copied() else {
            let _ = handle
                .approve(
                    &approval.request_id,
                    ToolApprovalDecision::timeout("approval client disconnected"),
                )
                .await;
            return;
        };
        self.state
            .set_pending_approval(thread_id, turn_id, approval.request_id.clone())
            .await;
        let response = self
            .outgoing
            .send_server_request_with_id_and_timeout(
                connection_id,
                RequestId::String(approval.request_id.clone()),
                ServerRequest::ApprovalRequest(approval.clone()),
                self.approval_request_timeout,
            )
            .await;
        let (decision, protocol_decision) = match response {
            Ok(value) => tool_approval_decision_from_response(value),
            Err(error) => (
                ToolApprovalDecision::timeout(error.message().to_string()),
                ApprovalDecision::Deny,
            ),
        };
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
                ServerNotification::ApprovalResolved(
                    crate::app_server::protocol::ApprovalResolveParams {
                        thread_id: thread_id.to_string(),
                        turn_id: turn_id.to_string(),
                        request_id: approval.request_id,
                        decision: protocol_decision,
                    },
                ),
            )
            .await;
    }
}

pub struct InterruptTurnOutcome {
    pub approval_resolved: Option<ApprovalResolveParams>,
    pub completed_turn: Option<AppTurn>,
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

fn item_from_notification(notification: &ServerNotification) -> Option<AppItem> {
    match notification {
        ServerNotification::AgentMessageDelta(AgentMessageDeltaParams {
            item_id, delta, ..
        }) => Some(AppItem {
            id: item_id.clone(),
            run_event_id: item_id.clone(),
            kind: AppItemKind::AgentMessage,
            status: AppItemStatus::Completed,
            created_at_ms: timestamp_millis(),
            completed_at_ms: Some(timestamp_millis()),
            content: Some(json!({ "text": delta })),
        }),
        ServerNotification::ItemStarted(ItemStartedParams { item, .. })
        | ServerNotification::ItemCompleted(ItemCompletedParams { item, .. }) => Some(item.clone()),
        ServerNotification::ApprovalRequested(ApprovalRequestParams {
            request_id,
            tool_name,
            preview,
            ..
        }) => Some(AppItem {
            id: request_id.clone(),
            run_event_id: request_id.clone(),
            kind: AppItemKind::ApprovalRequest,
            status: AppItemStatus::InProgress,
            created_at_ms: timestamp_millis(),
            completed_at_ms: None,
            content: Some(json!({
                "toolName": tool_name,
                "preview": preview,
                "choices": [ApprovalDecision::Allow, ApprovalDecision::Deny],
            })),
        }),
        _ => None,
    }
}

fn is_terminal_turn_notification(notification: &ServerNotification) -> bool {
    matches!(notification, ServerNotification::TurnCompleted(_))
}

fn interrupted_turn(mut turn: AppTurn) -> AppTurn {
    turn.status = TurnStatus::Interrupted;
    turn.completed_at_ms = Some(timestamp_millis());
    turn
}

fn input_text(input: &[UserInput]) -> String {
    input
        .iter()
        .map(|item| item.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn store_error(error: ThreadStoreError) -> AppServerError {
    AppServerError::internal(error.to_string())
}

fn tool_approval_decision_from_response(
    value: serde_json::Value,
) -> (ToolApprovalDecision, ApprovalDecision) {
    match value
        .get("decision")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
    {
        "allow" | "approved" => (ToolApprovalDecision::allow(), ApprovalDecision::Allow),
        "deny" | "denied" => {
            let reason = value
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("approval denied");
            (ToolApprovalDecision::deny(reason), ApprovalDecision::Deny)
        }
        _ => (
            ToolApprovalDecision::deny("invalid approval response"),
            ApprovalDecision::Deny,
        ),
    }
}

fn timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}
