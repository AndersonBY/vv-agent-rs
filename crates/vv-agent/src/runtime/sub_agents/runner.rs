use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::llm::LlmClient;
use crate::runtime::sub_task_manager::SubTaskManager;
use crate::runtime::AgentRuntime;
use crate::tools::SubTaskRunner;
use crate::types::{AgentTask, SubTaskOutcome, SubTaskRequest};
use crate::workspace::{
    DiscoveryFilteredWorkspaceBackend, WorkspaceBackend, INVALID_EXCLUDE_FILES_PATTERN_CODE,
    INVALID_EXCLUDE_FILES_PATTERN_MESSAGE,
};

use super::events::{emit_sub_run_completed, emit_sub_run_started};
use super::task::build_sub_agent_task;
use super::types::{SubTaskBuildInputs, SubTaskRunContext, SubTaskRunControls};

mod identity;
mod model;
mod outcome;
mod session;

use identity::resolve_sub_task_identity;
use model::resolve_sub_agent_client;
use outcome::{
    failed_sub_task_outcome, failed_sub_task_outcome_with_code, record_sub_task_outcome,
};
use session::run_attached_sub_agent_session;

impl<C: LlmClient + Clone + 'static> AgentRuntime<C> {
    pub(in crate::runtime) fn build_sub_task_runner(
        &self,
        parent_task: &AgentTask,
        workspace_path: PathBuf,
        workspace_backend: Arc<dyn WorkspaceBackend>,
        parent_shared_state: BTreeMap<String, Value>,
        sub_task_manager: SubTaskManager,
        controls: SubTaskRunControls,
    ) -> Option<SubTaskRunner> {
        if parent_task.sub_agents.is_empty() {
            return None;
        }
        let llm_client: Arc<dyn LlmClient> = Arc::new(self.llm_client.clone());
        let tool_registry = self.tool_registry.clone();
        let parent_task = parent_task.clone();
        let sub_task_context = SubTaskRunContext {
            llm_client,
            tool_registry,
            workspace_backend,
            workspace_path,
            parent_task,
            parent_shared_state,
            sub_task_manager,
            parent_cancellation_token: controls.parent_cancellation_token,
            settings_file: self.settings_file.clone(),
            default_backend: self.default_backend.clone(),
            sub_agent_timeout_seconds: self.sub_agent_timeout_seconds,
            event_handler: controls.event_handler,
            parent_execution_context: controls.parent_execution_context,
            model_provider: controls.model_provider,
            parent_run_context: controls.parent_run_context,
            tool_policy: controls.tool_policy,
            budget_limits: controls.budget_limits,
        };
        Some(Arc::new(move |request| {
            run_sub_task(sub_task_context.clone(), request)
        }))
    }
}

fn run_sub_task(context: SubTaskRunContext, request: SubTaskRequest) -> SubTaskOutcome {
    let parent_task = &context.parent_task;
    let mut lifecycle = resolve_sub_task_identity(&context, &request);

    let Some(sub_agent) = context.parent_task.sub_agents.get(&request.agent_name) else {
        let available = context
            .parent_task
            .sub_agents
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        let outcome = failed_sub_task_outcome(
            &lifecycle.task_id,
            &request.agent_name,
            &lifecycle.session_id,
            format!(
                "Unknown sub-agent {:?}. Available: {available}",
                request.agent_name
            ),
        );
        return record_sub_task_outcome(
            &context,
            &lifecycle,
            context.workspace_backend.clone(),
            outcome,
        );
    };

    let workspace_backend: Arc<dyn WorkspaceBackend> = match request
        .exclude_files_pattern
        .as_deref()
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
    {
        Some(pattern) => {
            match DiscoveryFilteredWorkspaceBackend::new(context.workspace_backend.clone(), pattern)
            {
                Ok(filtered) => Arc::new(filtered),
                Err(_) => {
                    let outcome = failed_sub_task_outcome_with_code(
                        &lifecycle.task_id,
                        &request.agent_name,
                        &lifecycle.session_id,
                        INVALID_EXCLUDE_FILES_PATTERN_MESSAGE,
                        Some(INVALID_EXCLUDE_FILES_PATTERN_CODE),
                    );
                    return record_sub_task_outcome(
                        &context,
                        &lifecycle,
                        context.workspace_backend.clone(),
                        outcome,
                    );
                }
            }
        }
        None => context.workspace_backend.clone(),
    };
    let mut child_context = context.clone();
    child_context.workspace_backend = workspace_backend.clone();

    lifecycle.model = sub_agent.model.trim().to_string();
    if let Err(error) = emit_sub_run_started(&context.event_handler, &lifecycle) {
        return complete_failed_sub_run(&child_context, &lifecycle, error, None);
    }

    let execution = catch_unwind(AssertUnwindSafe(|| {
        if let Err(error) = sub_agent.validate() {
            return Err((error.message().to_string(), Some(error.code().to_string())));
        }

        let resolved_client = resolve_sub_agent_client(&child_context, parent_task, sub_agent)
            .map_err(|error| (error, None))?;
        lifecycle.model = resolved_client.model_id.clone();

        let sub_task = build_sub_agent_task(
            &child_context,
            SubTaskBuildInputs {
                lifecycle: &lifecycle,
                sub_agent,
                resolved_model_id: &resolved_client.model_id,
                resolved_native_multimodal: resolved_client.native_multimodal,
                resolved_context_length: resolved_client.context_length,
                resolved_max_output_tokens: resolved_client.max_output_tokens,
                request: &request,
            },
        );

        run_attached_sub_agent_session(
            &child_context,
            &request,
            &lifecycle,
            sub_task,
            resolved_client,
        )
        .map_err(|error| (error, None))
    }));
    let outcome = match execution {
        Ok(Ok(outcome)) => outcome,
        Ok(Err((error, error_code))) => {
            return complete_failed_sub_run(
                &child_context,
                &lifecycle,
                error,
                error_code.as_deref(),
            );
        }
        Err(payload) => {
            return complete_failed_sub_run(
                &child_context,
                &lifecycle,
                panic_payload_to_string(payload.as_ref()),
                None,
            );
        }
    };
    record_sub_task_outcome(&child_context, &lifecycle, workspace_backend, outcome)
}

fn complete_failed_sub_run(
    context: &SubTaskRunContext,
    lifecycle: &super::types::SubRunLifecycle,
    error: impl Into<String>,
    error_code: Option<&str>,
) -> SubTaskOutcome {
    let mut outcome = failed_sub_task_outcome_with_code(
        &lifecycle.task_id,
        &lifecycle.agent_name,
        &lifecycle.session_id,
        error,
        error_code,
    );
    if let Err(sink_error) = emit_sub_run_completed(
        &context.event_handler,
        lifecycle,
        &outcome,
        None,
        None,
        None,
    ) {
        outcome = failed_sub_task_outcome(
            &lifecycle.task_id,
            &lifecycle.agent_name,
            &lifecycle.session_id,
            sink_error,
        );
    }
    record_sub_task_outcome(
        context,
        lifecycle,
        context.workspace_backend.clone(),
        outcome,
    )
}

fn panic_payload_to_string(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    "configured sub-agent panicked before session execution".to_string()
}

#[cfg(test)]
mod parity_event_tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use serde_json::{json, Value};

    use super::super::types::SubTaskRunContext;
    use super::run_sub_task;
    use crate::events::RunEventPayload;
    use crate::llm::ScriptedLlmClient;
    use crate::runtime::sub_task_manager::SubTaskManager;
    use crate::runtime::{ExecutionContext, RunEventHandler};
    use crate::tools::{build_default_registry, FunctionTool, Tool, ToolOutput};
    use crate::types::{
        AgentStatus, AgentTask, LLMResponse, SubAgentConfig, SubTaskRequest, ToolCall,
    };
    use crate::workspace::MemoryWorkspaceBackend;
    use crate::RunContext;

    fn event_fixture() -> Vec<Value> {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/parity/configured_sub_agent_events.jsonl"
        ))
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("configured sub-agent event fixture"))
        .collect()
    }

    fn contract_fixture() -> Value {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/parity/configured_sub_agent.json"
        )))
        .expect("configured sub-agent contract fixture")
    }

    fn lifecycle_handler(events: Arc<Mutex<Vec<Value>>>) -> RunEventHandler {
        Arc::new(move |event| {
            if matches!(
                event.payload(),
                RunEventPayload::SubRunStarted { .. } | RunEventPayload::SubRunCompleted { .. }
            ) {
                events
                    .lock()
                    .expect("lifecycle events")
                    .push(serde_json::to_value(event).expect("lifecycle event wire"));
            }
        })
    }

    fn context(
        llm: ScriptedLlmClient,
        parent_task: AgentTask,
        event_handler: RunEventHandler,
    ) -> SubTaskRunContext {
        SubTaskRunContext {
            llm_client: Arc::new(llm),
            tool_registry: build_default_registry(),
            workspace_backend: Arc::new(MemoryWorkspaceBackend::default()),
            workspace_path: PathBuf::from("/contract-workspace"),
            parent_task,
            parent_shared_state: BTreeMap::new(),
            sub_task_manager: SubTaskManager::default(),
            parent_cancellation_token: None,
            settings_file: None,
            default_backend: None,
            sub_agent_timeout_seconds: 30.0,
            event_handler: Some(event_handler),
            parent_execution_context: Some(ExecutionContext {
                metadata: BTreeMap::from([
                    ("_vv_agent_run_id".to_string(), json!("parent-run")),
                    ("_vv_agent_trace_id".to_string(), json!("trace-parity")),
                ]),
                ..ExecutionContext::default()
            }),
            model_provider: None,
            parent_run_context: Some(RunContext {
                run_id: "parent-run".to_string(),
                agent_name: "parent".to_string(),
                ..RunContext::default()
            }),
            tool_policy: None,
            budget_limits: None,
        }
    }

    #[test]
    fn real_configured_sub_agent_events_normalize_to_shared_fixture() {
        let mut finish_arguments = BTreeMap::new();
        finish_arguments.insert("message".to_string(), json!("child done"));
        let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "child-finish",
                "task_finish",
                finish_arguments,
            )],
        )]);
        let mut parent_task =
            AgentTask::new("parent-task", "child-model", "Parent prompt", "Parent task");
        parent_task.sub_agents.insert(
            "researcher".to_string(),
            SubAgentConfig::new("child-model", "Research"),
        );
        let mapped_events = Arc::new(Mutex::new(Vec::new()));
        let mapped_events_for_handler = mapped_events.clone();
        let event_handler: RunEventHandler = Arc::new(move |event| {
            mapped_events_for_handler
                .lock()
                .expect("mapped events")
                .push(event.clone());
        });
        let success_context = context(llm, parent_task, event_handler.clone());
        let mut request = SubTaskRequest::new("researcher", "Collect facts");
        request.metadata = BTreeMap::from([
            ("parent_run_id".to_string(), json!("parent-run")),
            ("parent_tool_call_id".to_string(), json!("delegate")),
        ]);

        let outcome = run_sub_task(success_context, request);

        assert_eq!(outcome.status, AgentStatus::Completed);

        let mut invalid_parent_task =
            AgentTask::new("parent-task", "child-model", "Parent prompt", "Parent task");
        let mut invalid_sub_agent = SubAgentConfig::new("child-model", "Research");
        invalid_sub_agent.system_prompt = Some(" \n ".to_string());
        invalid_parent_task
            .sub_agents
            .insert("researcher".to_string(), invalid_sub_agent);
        let invalid_context = context(
            ScriptedLlmClient::new(Vec::new()),
            invalid_parent_task,
            event_handler,
        );
        let mut invalid_request = SubTaskRequest::new("researcher", "Collect facts");
        invalid_request.metadata = BTreeMap::from([
            ("parent_run_id".to_string(), json!("parent-run")),
            ("parent_tool_call_id".to_string(), json!("delegate-failed")),
        ]);

        let invalid_outcome = run_sub_task(invalid_context, invalid_request);

        assert_eq!(invalid_outcome.status, AgentStatus::Failed);
        assert_eq!(
            invalid_outcome.error_code.as_deref(),
            Some("invalid_sub_agent_system_prompt")
        );
        let mapped_events = mapped_events.lock().expect("mapped events");
        let raw_events = mapped_events
            .iter()
            .filter(|event| {
                matches!(
                    event.payload(),
                    crate::events::RunEventPayload::SubRunStarted { .. }
                        | crate::events::RunEventPayload::SubRunCompleted { .. }
                )
            })
            .map(|event| serde_json::to_value(event).expect("serialize raw run event"))
            .collect::<Vec<_>>();
        for (pair, outcome) in raw_events.chunks_exact(2).zip([&outcome, &invalid_outcome]) {
            assert_eq!(
                pair[0]["parent_tool_call_id"],
                pair[1]["parent_tool_call_id"]
            );
            assert_eq!(pair[0]["task_id"], json!(outcome.task_id));
            assert_eq!(pair[1]["task_id"], json!(outcome.task_id));
            assert_eq!(pair[0]["session_id"], json!(outcome.session_id));
            assert_eq!(pair[1]["session_id"], json!(outcome.session_id));
            assert_eq!(pair[0]["child_session_id"], pair[0]["session_id"]);
            assert_eq!(pair[1]["child_session_id"], pair[1]["session_id"]);
        }
        let actual = raw_events
            .into_iter()
            .map(|mut value| {
                let failed_pair = value["parent_tool_call_id"] == "delegate-failed";
                let task_id = if failed_pair {
                    "child-task-failed"
                } else {
                    "child-task"
                };
                let session_id = if failed_pair {
                    "child-session-failed"
                } else {
                    "child-session"
                };
                value["event_id"] = json!("evt_dynamic");
                value["run_id"] = json!("run_dynamic");
                value["session_id"] = json!(session_id);
                value["child_session_id"] = json!(session_id);
                value["task_id"] = json!(task_id);
                value["created_at"] = json!(0.0);
                value
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, event_fixture());
    }

    #[test]
    fn model_resolution_failure_still_pairs_started_and_completed() {
        let mut parent_task = AgentTask::new(
            "parent-task",
            "parent-model",
            "Parent prompt",
            "Parent task",
        );
        parent_task.sub_agents.insert(
            "researcher".to_string(),
            SubAgentConfig::new("child-model", "Research"),
        );
        let lifecycle = Arc::new(Mutex::new(Vec::new()));
        let event_handler = lifecycle_handler(lifecycle.clone());
        let context = context(
            ScriptedLlmClient::new(Vec::new()),
            parent_task,
            event_handler,
        );
        let mut request = SubTaskRequest::new("researcher", "Collect facts");
        request.metadata = BTreeMap::from([
            ("task_id".to_string(), json!("child-task")),
            ("session_id".to_string(), json!("child-session")),
            ("parent_run_id".to_string(), json!("parent-run")),
            ("parent_tool_call_id".to_string(), json!("delegate")),
        ]);

        let outcome = run_sub_task(context, request);

        assert_eq!(outcome.status, AgentStatus::Failed);
        let lifecycle = lifecycle.lock().expect("lifecycle events");
        assert_eq!(
            lifecycle
                .iter()
                .map(|event| event["type"].as_str().expect("event type"))
                .collect::<Vec<_>>(),
            vec!["sub_run_started", "sub_run_completed"]
        );
        assert_eq!(lifecycle[1]["status"], "failed");
        assert_eq!(
            lifecycle[1]["metadata"]["error_code"],
            contract_fixture()["lifecycle"]["failure_error_code_fallback"]
        );
        assert!(lifecycle[1].get("token_usage").is_none());
        assert_eq!(lifecycle[0]["run_id"], lifecycle[1]["run_id"]);
    }

    #[test]
    fn validation_failure_maps_error_code_and_omits_unavailable_usage() {
        let mut parent_task =
            AgentTask::new("parent-task", "child-model", "Parent prompt", "Parent task");
        parent_task.sub_agents.insert(
            "researcher".to_string(),
            SubAgentConfig::new(" ", "Research"),
        );
        let lifecycle = Arc::new(Mutex::new(Vec::new()));
        let event_handler = lifecycle_handler(lifecycle.clone());
        let context = context(
            ScriptedLlmClient::new(Vec::new()),
            parent_task,
            event_handler,
        );
        let mut request = SubTaskRequest::new("researcher", "Collect facts");
        request.metadata = BTreeMap::from([
            ("task_id".to_string(), json!("child-task")),
            ("session_id".to_string(), json!("child-session")),
            ("parent_run_id".to_string(), json!("parent-run")),
            ("parent_tool_call_id".to_string(), json!("delegate")),
        ]);

        let outcome = run_sub_task(context, request);

        assert_eq!(outcome.status, AgentStatus::Failed);
        assert_eq!(
            outcome.error_code.as_deref(),
            Some("invalid_sub_agent_model")
        );
        let lifecycle = lifecycle.lock().expect("lifecycle events");
        assert_eq!(lifecycle.len(), 2);
        assert_eq!(
            lifecycle[1]["metadata"]["error_code"],
            "invalid_sub_agent_model"
        );
        assert!(lifecycle[1].get("token_usage").is_none());
    }

    #[test]
    fn session_setup_failure_still_pairs_started_and_completed() {
        let mut parent_task =
            AgentTask::new("parent-task", "child-model", "Parent prompt", "Parent task");
        parent_task.sub_agents.insert(
            "researcher".to_string(),
            SubAgentConfig::new("child-model", "Research"),
        );
        let lifecycle = Arc::new(Mutex::new(Vec::new()));
        let event_handler = lifecycle_handler(lifecycle.clone());
        let context = context(
            ScriptedLlmClient::new(Vec::new()),
            parent_task,
            event_handler,
        );
        let mut request = SubTaskRequest::new("researcher", "   ");
        request.metadata = BTreeMap::from([
            ("task_id".to_string(), json!("child-task")),
            ("session_id".to_string(), json!("child-session")),
            ("parent_run_id".to_string(), json!("parent-run")),
            ("parent_tool_call_id".to_string(), json!("delegate")),
        ]);

        let outcome = run_sub_task(context, request);

        assert_eq!(outcome.status, AgentStatus::Failed);
        assert_eq!(
            outcome.error.as_deref(),
            Some("Follow-up prompt cannot be empty.")
        );
        let lifecycle = lifecycle.lock().expect("lifecycle events");
        assert_eq!(
            lifecycle
                .iter()
                .map(|event| event["type"].as_str().expect("event type"))
                .collect::<Vec<_>>(),
            vec!["sub_run_started", "sub_run_completed"]
        );
        assert_eq!(lifecycle[1]["status"], "failed");
        assert_eq!(
            lifecycle[1]["metadata"]["error_code"],
            contract_fixture()["lifecycle"]["failure_error_code_fallback"]
        );
        assert!(lifecycle[1].get("token_usage").is_none());
        assert_eq!(lifecycle[0]["run_id"], lifecycle[1]["run_id"]);
    }

    #[test]
    fn terminal_wait_max_cycles_and_cancel_emit_matching_completion() {
        let cases = [
            (
                "wait_user",
                ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "approval-call",
                        "approval_action",
                        json!({"scope": "child"}),
                    )],
                )]),
                false,
            ),
            (
                "max_cycles",
                ScriptedLlmClient::new(vec![LLMResponse::new("keep going")]),
                false,
            ),
            ("failed", ScriptedLlmClient::new(Vec::new()), true),
        ];

        for (expected_status, llm, cancel_parent) in cases {
            let mut parent_task =
                AgentTask::new("parent-task", "child-model", "Parent prompt", "Parent task");
            let mut sub_agent = SubAgentConfig::new("child-model", "Research");
            sub_agent.max_cycles = 1;
            if expected_status == "wait_user" {
                parent_task
                    .extra_tool_names
                    .push("approval_action".to_string());
            }
            parent_task
                .sub_agents
                .insert("researcher".to_string(), sub_agent);
            let lifecycle = Arc::new(Mutex::new(Vec::new()));
            let event_handler = lifecycle_handler(lifecycle.clone());
            let mut context = context(llm, parent_task, event_handler);
            if expected_status == "wait_user" {
                let approval_tool = FunctionTool::builder("approval_action")
                    .needs_approval(true)
                    .json_schema(json!({
                        "type": "object",
                        "properties": {
                            "scope": {"type": "string"}
                        },
                        "required": []
                    }))
                    .handler(|_context, _arguments: Value| async {
                        Ok(ToolOutput::text("approved"))
                    })
                    .build()
                    .expect("approval tool");
                context
                    .tool_registry
                    .register(approval_tool.as_tool_spec())
                    .expect("register approval tool");
            }
            if cancel_parent {
                let token = crate::runtime::CancellationToken::default();
                token.cancel();
                context.parent_cancellation_token = Some(token);
            }
            let mut request = SubTaskRequest::new("researcher", "Collect facts");
            request.metadata = BTreeMap::from([
                ("task_id".to_string(), json!("child-task")),
                ("session_id".to_string(), json!("child-session")),
                ("parent_run_id".to_string(), json!("parent-run")),
                ("parent_tool_call_id".to_string(), json!("delegate")),
            ]);

            let outcome = run_sub_task(context, request);

            assert_eq!(
                super::super::events::agent_status_value(outcome.status),
                expected_status
            );
            let lifecycle = lifecycle.lock().expect("lifecycle events");
            assert_eq!(lifecycle.len(), 2);
            assert_eq!(lifecycle[0]["type"], "sub_run_started");
            assert_eq!(lifecycle[1]["type"], "sub_run_completed");
            assert_eq!(lifecycle[1]["status"], expected_status);
        }
    }
}
