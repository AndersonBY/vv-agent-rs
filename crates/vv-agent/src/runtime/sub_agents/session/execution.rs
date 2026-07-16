use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::Value;

use crate::budget::{BudgetExhaustion, BudgetUsageSnapshot};
use crate::runtime::sub_agents::events::{
    canonicalize_sub_agent_stream_event, emit_parent_sub_agent_event,
    emit_parent_sub_agent_stream_event, emit_sub_agent_session_event, emit_sub_run_completed,
    emit_sub_run_completed_to_log, emit_sub_run_started, enrich_sub_agent_payload,
};
use crate::runtime::sub_task_manager::SubTaskTurnSnapshot;
use crate::runtime::{
    AgentRuntime, CancellationToken, ExecutionContext, RuntimeRunControls, StreamCallback,
};
use crate::tools::ToolPolicy;
use crate::types::{AgentResult, AgentStatus, SubTaskOutcome, TaskTokenUsage};
use crate::RunContext;

mod progress;

use super::projection::project_execution_context;
use super::RuntimeSubAgentSession;
use crate::runtime::sub_agents::task::canonical_sub_run_metadata;
use progress::ObservedRunProgress;

type RunPromptError = Box<(String, Option<TaskTokenUsage>)>;
type RunPromptSuccess = (
    SubTaskOutcome,
    Option<TaskTokenUsage>,
    Option<BudgetUsageSnapshot>,
    Option<BudgetExhaustion>,
);

impl RuntimeSubAgentSession {
    pub(super) fn run_prompt(
        &self,
        prompt: &str,
        snapshot: Option<SubTaskTurnSnapshot>,
    ) -> Result<SubTaskOutcome, String> {
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err("Follow-up prompt cannot be empty.".to_string());
        }
        let controls = self.effective_turn_controls(snapshot.as_ref());
        let cancellation_token =
            self.begin_active_run(controls.parent_cancellation_token.as_ref())?;
        let _active_run_guard = ActiveRunGuard { session: self };
        let (lifecycle, started_before_session) = self.next_lifecycle(snapshot.as_ref());
        if !started_before_session {
            if let Err(error) = emit_sub_run_started(
                &self.parent_log_handler,
                &controls.event_handler,
                &lifecycle,
            ) {
                let outcome = self.failed_outcome(error, 0);
                return Ok(self.finish_outcome(&controls, &lifecycle, outcome, None, None, None));
            }
        }
        let observed_progress = Arc::new(ObservedRunProgress::default());
        let outcome = match catch_unwind(AssertUnwindSafe(|| {
            self.run_prompt_inner(
                prompt,
                &lifecycle,
                cancellation_token,
                &controls,
                observed_progress.clone(),
            )
        })) {
            Ok(Ok((outcome, token_usage, budget_usage, budget_exhaustion))) => self.finish_outcome(
                &controls,
                &lifecycle,
                outcome,
                token_usage.as_ref(),
                budget_usage.as_ref(),
                budget_exhaustion.as_ref(),
            ),
            Ok(Err(error)) => {
                let (error, token_usage) = *error;
                let (observed_cycles, observed_usage) = observed_progress.snapshot();
                let outcome = self.failed_outcome(error, observed_cycles);
                let token_usage = token_usage.as_ref().or(observed_usage.as_ref());
                self.finish_outcome(&controls, &lifecycle, outcome, token_usage, None, None)
            }
            Err(payload) => {
                let (observed_cycles, token_usage) = observed_progress.snapshot();
                let outcome =
                    self.failed_outcome(panic_payload_to_string(payload.as_ref()), observed_cycles);
                self.finish_outcome(
                    &controls,
                    &lifecycle,
                    outcome,
                    token_usage.as_ref(),
                    None,
                    None,
                )
            }
        };
        Ok(outcome)
    }

    fn run_prompt_inner(
        &self,
        prompt: &str,
        lifecycle: &super::super::types::SubRunLifecycle,
        cancellation_token: CancellationToken,
        controls: &EffectiveTurnControls,
        observed_progress: Arc<ObservedRunProgress>,
    ) -> Result<RunPromptSuccess, RunPromptError> {
        let (initial_messages, shared_state) = {
            let state = self.state.lock().map_err(|_| {
                Box::new((
                    "Sub-agent session state lock is poisoned.".to_string(),
                    None,
                ))
            })?;
            (state.messages.clone(), state.shared_state.clone())
        };
        self.emit(
            "session_run_start",
            BTreeMap::from([
                ("prompt".to_string(), Value::String(prompt.to_string())),
                (
                    "existing_messages".to_string(),
                    Value::from(initial_messages.len() as u64),
                ),
            ]),
        );

        let mut task = self.task_template.clone();
        task.user_prompt = prompt.to_string();
        task.initial_messages = initial_messages;
        task.initial_shared_state = shared_state;
        task.metadata = canonical_sub_run_metadata(&task.metadata, lifecycle);
        crate::runtime::tool_planner::project_tool_policy(&mut task, &controls.tool_policy);

        let listeners = self.listeners.clone();
        let parent_log_handler = self.parent_log_handler.clone();
        let parent_event_handler = controls.event_handler.clone();
        let task_id = self.task_id.clone();
        let session_id = self.session_id.clone();
        let agent_name = self.agent_name.clone();
        let observed_progress_for_log = observed_progress.clone();
        let log_handler = Arc::new(move |event: &str, payload: &BTreeMap<String, Value>| {
            if event == "cycle_llm_response" {
                observed_progress_for_log.record_completed_cycle(payload);
            }
            emit_sub_agent_session_event(&listeners, event, payload);
            let enriched = enrich_sub_agent_payload(payload, &task_id, &session_id, &agent_name);
            emit_parent_sub_agent_event(
                &parent_log_handler,
                &parent_event_handler,
                &format!("sub_agent_{event}"),
                enriched,
            );
        });
        let runtime = self.build_runtime(&controls.tool_policy);
        let mut execution_context = project_execution_context(
            controls.parent_execution_context.as_ref(),
            lifecycle,
            Some(cancellation_token.clone()),
        );
        if controls.stream_callback.is_some()
            || controls.event_handler.is_some()
            || self.parent_log_handler.is_some()
        {
            let callback = controls.stream_callback.clone();
            let parent_log_handler = self.parent_log_handler.clone();
            let parent_event_handler = controls.event_handler.clone();
            let lifecycle = lifecycle.clone();
            let stream_sequence = AtomicU64::new(1);
            let stream_callback: StreamCallback = Arc::new(move |event| {
                let Some(canonical) = canonicalize_sub_agent_stream_event(event, &lifecycle) else {
                    return;
                };
                emit_parent_sub_agent_stream_event_preserving_handler_panic(
                    &parent_log_handler,
                    &parent_event_handler,
                    &canonical,
                    stream_sequence.fetch_add(1, Ordering::Relaxed),
                );
                if let Some(callback) = callback.as_ref() {
                    emit_inherited_stream_observer(callback, &canonical);
                }
            });
            execution_context.stream_callback = Some(stream_callback);
        }
        let child_run_context = self.child_run_context(lifecycle, &task, &execution_context);
        let result = runtime
            .run_with_controls(
                task,
                RuntimeRunControls {
                    log_handler: Some(log_handler),
                    before_cycle_messages: None,
                    interruption_messages: None,
                    steering_queue: Some(self.steering_queue.clone()),
                    cancellation_token: Some(cancellation_token),
                    execution_context: Some(execution_context),
                    workspace: None,
                    workspace_backend: None,
                    model_provider: self.model_provider.clone(),
                    run_context: Some(child_run_context),
                    sub_task_manager: None,
                    budget_limits: self.budget_limits.clone(),
                    host_cost_meter: None,
                    background_parent_run_config: None,
                    initial_messages: None,
                    initial_shared_state: None,
                    initial_cycles: None,
                    cycle_index_start: None,
                    cycle_count: None,
                    initial_budget_usage: None,
                    defer_terminal_on_max_cycles: false,
                },
            )
            .map_err(|error| Box::new((error.to_string(), observed_progress.token_usage())))?;
        let token_usage = if result.cycles.is_empty() {
            (result.status != AgentStatus::Failed).then(|| result.token_usage.clone())
        } else {
            Some(crate::runtime::summarize_task_token_usage(&result.cycles))
        };

        {
            let mut state = self.state.lock().map_err(|_| {
                Box::new((
                    "Sub-agent session state lock is poisoned.".to_string(),
                    token_usage.clone(),
                ))
            })?;
            state.messages = result.messages.clone();
            state.shared_state = result.shared_state.clone();
        }
        self.emit_session_run_end(&result);
        let budget_usage = result.budget_usage.clone();
        let budget_exhaustion = result.budget_exhaustion.clone();
        Ok((
            self.outcome_from_result(result),
            token_usage,
            budget_usage,
            budget_exhaustion,
        ))
    }

    fn build_runtime(
        &self,
        tool_policy: &ToolPolicy,
    ) -> AgentRuntime<Arc<dyn crate::llm::LlmClient>> {
        let mut runtime = AgentRuntime::new(self.llm_client.clone())
            .with_tool_registry(self.tool_registry.clone())
            .with_tool_policy(tool_policy.clone());
        runtime.default_workspace = Some(self.workspace_path.clone());
        runtime.workspace_backend = self.workspace_backend.clone();
        runtime.settings_file = self.settings_file.clone();
        runtime.default_backend = self.default_backend.clone();
        runtime
    }

    pub(super) fn cancel_active_run(&self) -> bool {
        let cancellation_token = self
            .active_cancellation_token
            .lock()
            .ok()
            .and_then(|active| active.clone());
        let Some(cancellation_token) = cancellation_token else {
            return false;
        };
        cancellation_token.cancel();
        if let Ok(mut queue) = self.steering_queue.lock() {
            queue.clear();
        }
        self.emit("session_cancel_requested", BTreeMap::new());
        true
    }

    fn begin_active_run(
        &self,
        parent_cancellation_token: Option<&CancellationToken>,
    ) -> Result<CancellationToken, String> {
        let mut running = self
            .running
            .lock()
            .map_err(|_| "Sub-agent session running lock is poisoned.".to_string())?;
        if *running {
            return Err("Sub-agent session is already running.".to_string());
        }
        let cancellation_token = parent_cancellation_token
            .map(CancellationToken::child)
            .unwrap_or_default();
        *self
            .active_cancellation_token
            .lock()
            .map_err(|_| "Sub-agent session cancellation lock is poisoned.".to_string())? =
            Some(cancellation_token.clone());
        *running = true;
        Ok(cancellation_token)
    }

    fn finish_active_run(&self) {
        if let Ok(mut active) = self.active_cancellation_token.lock() {
            *active = None;
        }
        if let Ok(mut running) = self.running.lock() {
            *running = false;
        }
    }

    fn next_lifecycle(
        &self,
        snapshot: Option<&SubTaskTurnSnapshot>,
    ) -> (super::super::types::SubRunLifecycle, bool) {
        if self.initial_lifecycle_pending.swap(false, Ordering::SeqCst) {
            return (self.lifecycle_template.clone(), true);
        }
        let mut lifecycle = self.lifecycle_template.clone();
        lifecycle.run_id = format!("run_{}", uuid::Uuid::new_v4().simple());
        if let Some(snapshot) = snapshot {
            lifecycle.trace_id = snapshot
                .trace_id
                .clone()
                .unwrap_or_else(|| lifecycle.run_id.clone());
            lifecycle.parent_run_id = snapshot.parent_run_id.clone().unwrap_or_default();
            lifecycle.parent_tool_call_id =
                snapshot.parent_tool_call_id.clone().unwrap_or_default();
        }
        (lifecycle, false)
    }

    fn effective_turn_controls(
        &self,
        snapshot: Option<&SubTaskTurnSnapshot>,
    ) -> EffectiveTurnControls {
        match snapshot {
            Some(snapshot) => {
                let mut tool_policy = snapshot.tool_policy.clone();
                add_fixed_child_exclusions(&mut tool_policy, &self.task_template.exclude_tools);
                EffectiveTurnControls {
                    parent_cancellation_token: snapshot.cancellation_token.clone(),
                    event_handler: snapshot.event_handler.clone(),
                    stream_callback: snapshot.stream_callback.clone(),
                    parent_execution_context: snapshot.parent_execution_context.clone(),
                    tool_policy,
                }
            }
            None => EffectiveTurnControls {
                parent_cancellation_token: self.parent_cancellation_token.clone(),
                event_handler: self.parent_event_handler.clone(),
                stream_callback: self.stream_callback.clone(),
                parent_execution_context: self.parent_execution_context.clone(),
                tool_policy: self.tool_policy.clone(),
            },
        }
    }

    fn child_run_context(
        &self,
        lifecycle: &super::super::types::SubRunLifecycle,
        task: &crate::types::AgentTask,
        execution_context: &ExecutionContext,
    ) -> RunContext {
        let mut metadata = task.metadata.clone();
        if !lifecycle.parent_run_id.is_empty() {
            metadata.insert(
                "parent_run_id".to_string(),
                Value::String(lifecycle.parent_run_id.clone()),
            );
        }
        if !lifecycle.parent_tool_call_id.is_empty() {
            metadata.insert(
                "parent_tool_call_id".to_string(),
                Value::String(lifecycle.parent_tool_call_id.clone()),
            );
        }
        if !lifecycle.trace_id.is_empty() {
            metadata.insert(
                "trace_id".to_string(),
                Value::String(lifecycle.trace_id.clone()),
            );
        }
        RunContext {
            run_id: lifecycle.run_id.clone(),
            agent_name: lifecycle.agent_name.clone(),
            model: Some(self.run_model_ref.clone()),
            workspace: Some(self.workspace_path.clone()),
            metadata,
            app_state: execution_context.app_state.clone(),
        }
    }

    fn outcome_from_result(&self, result: AgentResult) -> SubTaskOutcome {
        let todo_list = result.todo_list();
        let cycles = result.cycles.len() as u32;
        let error_code =
            (result.status == AgentStatus::Failed).then(|| "sub_task_failed".to_string());
        SubTaskOutcome {
            task_id: self.task_id.clone(),
            agent_name: self.agent_name.clone(),
            status: result.status,
            session_id: Some(self.session_id.clone()),
            final_answer: result.final_answer,
            wait_reason: result.wait_reason,
            error: result.error,
            error_code,
            completion_reason: result.completion_reason,
            completion_tool_name: result.completion_tool_name,
            partial_output: result.partial_output,
            cycles,
            todo_list,
            resolved: self.resolved.clone(),
        }
    }

    fn failed_outcome(&self, error: impl Into<String>, cycles: u32) -> SubTaskOutcome {
        SubTaskOutcome {
            task_id: self.task_id.clone(),
            agent_name: self.agent_name.clone(),
            status: AgentStatus::Failed,
            session_id: Some(self.session_id.clone()),
            final_answer: None,
            wait_reason: None,
            error: Some(error.into()),
            error_code: Some("sub_task_failed".to_string()),
            completion_reason: Some(crate::types::CompletionReason::Failed),
            completion_tool_name: None,
            partial_output: None,
            cycles,
            todo_list: Vec::new(),
            resolved: self.resolved.clone(),
        }
    }

    fn finish_outcome(
        &self,
        controls: &EffectiveTurnControls,
        lifecycle: &super::super::types::SubRunLifecycle,
        outcome: SubTaskOutcome,
        token_usage: Option<&TaskTokenUsage>,
        budget_usage: Option<&BudgetUsageSnapshot>,
        budget_exhaustion: Option<&BudgetExhaustion>,
    ) -> SubTaskOutcome {
        match emit_sub_run_completed(
            &self.parent_log_handler,
            &controls.event_handler,
            lifecycle,
            &outcome,
            token_usage,
            budget_usage,
            budget_exhaustion,
        ) {
            Ok(()) => outcome,
            Err(error) => {
                let failed = self.failed_outcome(error, outcome.cycles);
                emit_sub_run_completed_to_log(
                    &self.parent_log_handler,
                    lifecycle,
                    &failed,
                    token_usage,
                    budget_usage,
                    budget_exhaustion,
                );
                failed
            }
        }
    }
}

struct EffectiveTurnControls {
    parent_cancellation_token: Option<CancellationToken>,
    event_handler: Option<crate::runtime::RuntimeEventHandler>,
    stream_callback: Option<StreamCallback>,
    parent_execution_context: Option<ExecutionContext>,
    tool_policy: ToolPolicy,
}

fn add_fixed_child_exclusions(tool_policy: &mut ToolPolicy, exclusions: &[String]) {
    for exclusion in exclusions {
        if !tool_policy
            .disallowed_tools
            .iter()
            .any(|tool| tool == exclusion)
        {
            tool_policy.disallowed_tools.push(exclusion.clone());
        }
    }
}

struct ActiveRunGuard<'a> {
    session: &'a RuntimeSubAgentSession,
}

impl Drop for ActiveRunGuard<'_> {
    fn drop(&mut self) {
        self.session.finish_active_run();
    }
}

fn panic_payload_to_string(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    "configured sub-agent panicked".to_string()
}

fn emit_parent_sub_agent_stream_event_preserving_handler_panic(
    parent_log_handler: &Option<crate::runtime::RuntimeLogHandler>,
    parent_event_handler: &Option<crate::runtime::RuntimeEventHandler>,
    canonical: &BTreeMap<String, Value>,
    sequence: u64,
) {
    if parent_event_handler.is_none() {
        emit_parent_sub_agent_stream_event(
            parent_log_handler,
            parent_event_handler,
            canonical,
            sequence,
        );
        return;
    }

    let event = canonical
        .get("event")
        .and_then(Value::as_str)
        .expect("canonical sub-agent stream event");
    let event = format!("sub_agent_{event}");
    let mut payload = canonical.clone();
    payload.remove("event");

    if let Some(handler) = parent_log_handler {
        if let Ok(mut handler) = handler.lock() {
            let _ = catch_unwind(AssertUnwindSafe(|| handler(&event, &payload)));
        }
    }
    if let Some(handler) = parent_event_handler {
        payload.insert(
            "_vv_agent_stream_receipt".to_string(),
            Value::String(format!("stream_{}", uuid::Uuid::new_v4().simple())),
        );
        payload.insert(
            "_vv_agent_stream_sequence".to_string(),
            Value::from(sequence),
        );
        handler(&event, &payload);
    }
}

fn emit_inherited_stream_observer(callback: &StreamCallback, canonical: &BTreeMap<String, Value>) {
    let _ = catch_unwind(AssertUnwindSafe(|| callback(canonical)));
}

#[cfg(test)]
mod capability_projection_tests {
    use std::any::Any;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use serde_json::json;

    use super::{project_execution_context, RuntimeSubAgentSession};
    use crate::approval::{ApprovalBroker, ApprovalFuture, ApprovalProvider, ApprovalRequest};
    use crate::llm::{LlmError, ScriptStep, ScriptedLlmClient};
    use crate::memory::{
        MemoryFuture, MemoryProvider, MemorySaveRequest, MemorySaveResult, MemorySearchRequest,
        MemorySearchResult,
    };
    use crate::model::{ModelProvider, ModelRef, ScriptedModelProvider};
    use crate::runtime::sub_agent_sessions::SubAgentSession;
    use crate::runtime::sub_agents::types::{RuntimeSubAgentSessionParts, SubRunLifecycle};
    use crate::runtime::{
        CancellationToken, ExecutionContext, InMemoryStateStore, RuntimeEventHandler, StateStore,
        StreamCallback,
    };
    use crate::tools::{build_default_registry, ApprovalDecision};
    use crate::types::{
        AgentResult, AgentStatus, AgentTask, CompletionReason, LLMResponse, TokenUsage,
    };
    use crate::workspace::MemoryWorkspaceBackend;

    struct AllowingApprovalProvider;

    impl ApprovalProvider for AllowingApprovalProvider {
        fn should_request(&self, _request: &ApprovalRequest) -> bool {
            false
        }

        fn decide(&self, _request: &ApprovalRequest) -> ApprovalFuture<Option<ApprovalDecision>> {
            Box::pin(async { Ok(Some(ApprovalDecision::Approved)) })
        }
    }

    struct EmptyMemoryProvider;

    impl MemoryProvider for EmptyMemoryProvider {
        fn search(&self, _request: MemorySearchRequest) -> MemoryFuture<Vec<MemorySearchResult>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn save(&self, _request: MemorySaveRequest) -> MemoryFuture<MemorySaveResult> {
            Box::pin(async { Ok(MemorySaveResult::default()) })
        }
    }

    fn runtime_session(
        parent_cancellation_token: Option<CancellationToken>,
    ) -> RuntimeSubAgentSession {
        runtime_session_with_client(
            ScriptedLlmClient::new(Vec::new()),
            parent_cancellation_token,
        )
    }

    fn runtime_session_with_client(
        llm_client: ScriptedLlmClient,
        parent_cancellation_token: Option<CancellationToken>,
    ) -> RuntimeSubAgentSession {
        RuntimeSubAgentSession::new(RuntimeSubAgentSessionParts {
            llm_client: Arc::new(llm_client),
            tool_registry: build_default_registry(),
            workspace_path: PathBuf::from("/contract/workspace"),
            workspace_backend: Arc::new(MemoryWorkspaceBackend::default()),
            task_template: AgentTask::new(
                "child-task",
                "child-model",
                "Child prompt",
                "Child task",
            ),
            agent_name: "researcher".to_string(),
            session_id: "child-session".to_string(),
            resolved: Default::default(),
            settings_file: None,
            default_backend: None,
            parent_cancellation_token,
            stream_callback: None,
            parent_log_handler: None,
            parent_event_handler: None,
            parent_execution_context: None,
            model_provider: None,
            run_model_ref: ModelRef::named("child-model"),
            tool_policy: crate::tools::ToolPolicy::default(),
            budget_limits: None,
            initial_lifecycle: SubRunLifecycle {
                run_id: "child-run".to_string(),
                trace_id: "trace-contract".to_string(),
                parent_run_id: "parent-run".to_string(),
                parent_tool_call_id: "delegate".to_string(),
                task_id: "child-task".to_string(),
                session_id: "child-session".to_string(),
                agent_name: "researcher".to_string(),
                parent_task_id: "parent-task".to_string(),
                model: "child-model".to_string(),
            },
        })
    }

    #[test]
    fn execution_error_reports_usage_only_after_a_completed_llm_cycle() {
        let contract: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/parity/configured_sub_agent_v1.json"
        )))
        .expect("configured sub-agent contract");
        let captured = Arc::new(Mutex::new(Vec::new()));
        let captured_for_handler = captured.clone();
        let event_handler: crate::runtime::RuntimeEventHandler = Arc::new(move |event, payload| {
            if event == "sub_run_completed" {
                captured_for_handler
                    .lock()
                    .expect("captured completions")
                    .push(payload.clone());
            }
        });

        let fail_immediately = ScriptedLlmClient::from_steps(vec![ScriptStep::callback(|_| {
            Err(LlmError::Request("first request failed".to_string()))
        })]);
        let mut immediate_session = runtime_session_with_client(fail_immediately, None);
        immediate_session.parent_event_handler = Some(event_handler.clone());

        let immediate = immediate_session
            .run_prompt("first attempt", None)
            .expect("failed child outcome");

        assert_eq!(immediate.status, AgentStatus::Failed);
        assert_eq!(
            !captured.lock().expect("captured completions")[0].contains_key("token_usage"),
            contract["lifecycle"]["omit_token_usage_when_unavailable"]
        );

        let mut first_response = LLMResponse::new("continue");
        first_response.token_usage = TokenUsage {
            prompt_tokens: 11,
            completion_tokens: 7,
            total_tokens: 18,
            input_tokens: 11,
            output_tokens: 7,
            ..TokenUsage::default()
        };
        let fail_after_cycle = ScriptedLlmClient::from_steps(vec![
            ScriptStep::response(first_response),
            ScriptStep::callback(|_| Err(LlmError::Request("second request failed".to_string()))),
        ]);
        let mut partial_session = runtime_session_with_client(fail_after_cycle, None);
        partial_session.parent_event_handler = Some(event_handler);

        let partial = partial_session
            .run_prompt("second attempt", None)
            .expect("partially executed child outcome");

        assert_eq!(partial.status, AgentStatus::Failed);
        assert_eq!(
            contract["lifecycle"]["preserve_failed_usage_after_completed_cycle"],
            true
        );
        let captured = captured.lock().expect("captured completions");
        assert_eq!(captured[1]["status"], "failed");
        assert_eq!(captured[1]["token_usage"]["prompt_tokens"], json!(11));
        assert_eq!(captured[1]["token_usage"]["completion_tokens"], json!(7));
        assert_eq!(captured[1]["token_usage"]["total_tokens"], json!(18));
        assert_eq!(
            captured[1]["token_usage"]["cycles"][0]["cycle_index"],
            json!(1)
        );
        assert_eq!(
            captured[1]["token_usage"]["cycles"][0]["usage"]["total_tokens"],
            json!(18)
        );
    }

    mod wait_user;

    #[test]
    fn session_cancel_targets_only_the_active_child_token_and_clears_after_run() {
        let parent = CancellationToken::default();
        let session = runtime_session(Some(parent.clone()));
        let child = session
            .begin_active_run(session.parent_cancellation_token.as_ref())
            .expect("begin child run");

        assert!(session.cancel());
        assert!(child.is_cancelled());
        assert!(!parent.is_cancelled());

        session.finish_active_run();
        assert!(!session.cancel());

        let independent_session = runtime_session(None);
        let independent = independent_session
            .begin_active_run(session.parent_cancellation_token.as_ref())
            .expect("begin independent run");
        assert!(independent_session.cancel());
        assert!(independent.is_cancelled());
        independent_session.finish_active_run();
    }

    #[test]
    fn child_execution_context_inherits_only_capabilities_and_derives_identity() {
        let contract: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/parity/configured_sub_agent_v1.json"
        )))
        .expect("configured sub-agent contract");
        let state_store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
        let approval_provider: Arc<dyn ApprovalProvider> = Arc::new(AllowingApprovalProvider);
        let memory_provider: Arc<dyn MemoryProvider> = Arc::new(EmptyMemoryProvider);
        let model_provider: Arc<dyn ModelProvider> = Arc::new(ScriptedModelProvider::default());
        let app_state: Arc<dyn Any + Send + Sync> = Arc::new("app-state".to_string());
        let event_sink: RuntimeEventHandler = Arc::new(|_, _| {});
        let stream_sink: StreamCallback = Arc::new(|_| {});
        let approval_broker = ApprovalBroker::default();
        let parent_token = CancellationToken::default();
        let child_token = parent_token.child();
        let parent = ExecutionContext {
            cancellation_token: Some(parent_token.clone()),
            state_store: Some(state_store.clone()),
            approval_provider: Some(approval_provider.clone()),
            approval_broker: Some(approval_broker.clone()),
            approval_timeout: Some(Duration::from_secs(7)),
            memory_providers: vec![memory_provider.clone()],
            app_state: Some(app_state.clone()),
            metadata: std::collections::BTreeMap::from([
                ("_vv_agent_run_id".to_string(), json!("parent-run")),
                ("_vv_agent_session_id".to_string(), json!("parent-session")),
                ("_vv_agent_input".to_string(), json!("parent input")),
                ("_vv_agent_session".to_string(), json!("parent session")),
                (
                    "_vv_agent_trace_context".to_string(),
                    json!({"traceparent": "00-contract"}),
                ),
                (
                    "trace_context".to_string(),
                    json!({"vendor": {"sampled": true}}),
                ),
                ("private_parent_value".to_string(), json!(true)),
            ]),
            ..ExecutionContext::default()
        };
        let lifecycle = SubRunLifecycle {
            run_id: "child-run".to_string(),
            trace_id: "trace-parity".to_string(),
            parent_run_id: "parent-run".to_string(),
            parent_tool_call_id: "delegate".to_string(),
            task_id: "child-task".to_string(),
            session_id: "child-session".to_string(),
            agent_name: "researcher".to_string(),
            parent_task_id: "parent-task".to_string(),
            model: "child-model".to_string(),
        };

        let child = project_execution_context(Some(&parent), &lifecycle, Some(child_token.clone()));
        let mut session = runtime_session(None);
        session.parent_event_handler = Some(event_sink.clone());
        session.stream_callback = Some(stream_sink.clone());
        session.model_provider = Some(model_provider.clone());
        let parent_run_context = crate::RunContext {
            run_id: "parent-run".to_string(),
            agent_name: "parent".to_string(),
            metadata: std::collections::BTreeMap::from([("parent_only".to_string(), json!(true))]),
            ..crate::RunContext::default()
        };
        let child_run_context =
            session.child_run_context(&lifecycle, &session.task_template, &child);

        assert!(Arc::ptr_eq(
            child.state_store.as_ref().expect("state store"),
            &state_store
        ));
        assert!(Arc::ptr_eq(
            child.approval_provider.as_ref().expect("approval provider"),
            &approval_provider
        ));
        assert!(Arc::ptr_eq(&child.memory_providers[0], &memory_provider));
        assert!(Arc::ptr_eq(
            child.app_state.as_ref().expect("app state"),
            &app_state
        ));
        assert_eq!(child.approval_timeout, Some(Duration::from_secs(7)));
        approval_broker
            .allow_tool_for_session("shared-tool")
            .expect("allow session tool");
        assert_eq!(
            child
                .approval_broker
                .as_ref()
                .expect("approval broker")
                .allows_tool_for_session("shared-tool"),
            Ok(true)
        );
        assert_eq!(child.metadata["_vv_agent_run_id"], "child-run");
        assert_eq!(child.metadata["_vv_agent_session_id"], "child-session");
        assert_eq!(child.metadata["_vv_agent_agent_name"], "researcher");
        assert_eq!(child.metadata["_vv_agent_trace_id"], "trace-parity");
        assert_eq!(child.metadata["_vv_agent_parent_run_id"], "parent-run");
        assert_eq!(child.metadata["_vv_agent_parent_tool_call_id"], "delegate");
        assert_eq!(
            child.metadata["_vv_agent_trace_context"],
            json!({"traceparent": "00-contract"})
        );
        assert_eq!(
            child.metadata["trace_context"],
            json!({"vendor": {"sampled": true}})
        );
        assert!(!child.metadata.contains_key("_vv_agent_input"));
        assert!(!child.metadata.contains_key("_vv_agent_session"));
        assert!(!child.metadata.contains_key("private_parent_value"));
        let inherited_checks = std::collections::BTreeMap::from([
            (
                "app_state",
                Arc::ptr_eq(child.app_state.as_ref().expect("app state"), &app_state),
            ),
            ("approval_broker", child.approval_broker.is_some()),
            (
                "approval_provider",
                Arc::ptr_eq(
                    child.approval_provider.as_ref().expect("approval provider"),
                    &approval_provider,
                ),
            ),
            (
                "approval_timeout",
                child.approval_timeout == Some(Duration::from_secs(7)),
            ),
            (
                "event_sink",
                Arc::ptr_eq(
                    session.parent_event_handler.as_ref().expect("event sink"),
                    &event_sink,
                ),
            ),
            (
                "memory_providers",
                Arc::ptr_eq(&child.memory_providers[0], &memory_provider),
            ),
            (
                "model_provider",
                Arc::ptr_eq(
                    session.model_provider.as_ref().expect("model provider"),
                    &model_provider,
                ),
            ),
            (
                "state_store",
                Arc::ptr_eq(
                    child.state_store.as_ref().expect("state store"),
                    &state_store,
                ),
            ),
            (
                "stream_sink",
                Arc::ptr_eq(
                    session.stream_callback.as_ref().expect("stream sink"),
                    &stream_sink,
                ),
            ),
            (
                "trace_context",
                child.metadata.get("_vv_agent_trace_context")
                    == Some(&json!({"traceparent": "00-contract"})),
            ),
        ]);
        let derived_checks = std::collections::BTreeMap::from([
            ("agent_name", child_run_context.agent_name == "researcher"),
            ("cancellation_token", child.cancellation_token.is_some()),
            (
                "run_context",
                child_run_context.run_id != parent_run_context.run_id,
            ),
            ("run_id", child_run_context.run_id == "child-run"),
            (
                "session_id",
                child.metadata.get("_vv_agent_session_id") == Some(&json!("child-session")),
            ),
        ]);
        let isolated_checks = std::collections::BTreeMap::from([
            ("input", !child.metadata.contains_key("_vv_agent_input")),
            (
                "parent_run_context",
                !child_run_context.metadata.contains_key("parent_only"),
            ),
            ("session", !child.metadata.contains_key("_vv_agent_session")),
            (
                "shared_state",
                !session
                    .state
                    .lock()
                    .expect("child state")
                    .shared_state
                    .contains_key("parent_secret"),
            ),
        ]);
        let lineage_checks = std::collections::BTreeMap::from([
            (
                "parent_run_id",
                child_run_context.metadata.get("parent_run_id") == Some(&json!("parent-run")),
            ),
            (
                "parent_tool_call_id",
                child_run_context.metadata.get("parent_tool_call_id") == Some(&json!("delegate")),
            ),
        ]);
        for (category, checks) in [
            ("inherited", inherited_checks),
            ("derived", derived_checks),
            ("isolated", isolated_checks),
            ("lineage", lineage_checks),
        ] {
            let expected = contract["capability_projection"][category]
                .as_array()
                .expect("capability category");
            assert_eq!(checks.len(), expected.len(), "{category} check count");
            for name in expected.iter().filter_map(serde_json::Value::as_str) {
                assert_eq!(
                    checks.get(name),
                    Some(&true),
                    "capability {category}.{name}"
                );
            }
        }
        child_token.cancel();
        assert!(!parent_token.is_cancelled());
    }
}
