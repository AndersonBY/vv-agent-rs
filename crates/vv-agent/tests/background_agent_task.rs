use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use vv_agent::{
    Agent, AgentStatus, ApprovalDecision, ApprovalFuture, ApprovalPolicy, ApprovalProvider,
    ApprovalRequest, ContextError, ContextFragment, ContextProvider, ContextRequest,
    EventStoreError, FunctionTool, GuardrailOutcome, InputGuardrail, LLMResponse, MemorySession,
    MemoryWorkspaceBackend, Message, ModelProvider, ModelRef, NormalizedInput, RunConfig,
    RunContext, RunEvent, RunEventIter, RunEventReplayQuery, RunEventStore, Runner, RuntimeHook,
    ScriptStep, ScriptedModelProvider, Span, StaticTool, ThreadBackend, Tool, ToolCall,
    ToolContext, ToolOutput, ToolPolicy, ToolResultStatus, ToolUseBehavior, TraceSink,
    WorkspaceBackend,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct ObservedContext {
    workspace: PathBuf,
    shared_state: BTreeMap<String, Value>,
    metadata: BTreeMap<String, Value>,
    app_state: Option<String>,
    inherited_workspace_backend: bool,
    inherited_model_provider: bool,
    thread_workers: Option<usize>,
}

fn finish_response(message: &str) -> LLMResponse {
    LLMResponse::with_tool_calls(
        "",
        vec![ToolCall::from_raw_arguments(
            "finish",
            "task_finish",
            json!({"message": message}),
        )],
    )
}

struct RejectBackgroundInput;

impl InputGuardrail for RejectBackgroundInput {
    fn check(
        &self,
        _context: &RunContext,
        _input: &NormalizedInput,
    ) -> GuardrailOutcome<NormalizedInput> {
        GuardrailOutcome::Block {
            message: "background input rejected".to_string(),
        }
    }
}

#[derive(Clone, Default)]
struct RecordingApprovalProvider {
    requests: Arc<Mutex<Vec<ApprovalRequest>>>,
}

impl ApprovalProvider for RecordingApprovalProvider {
    fn should_request(&self, _request: &ApprovalRequest) -> bool {
        true
    }

    fn decide(&self, request: &ApprovalRequest) -> ApprovalFuture<Option<ApprovalDecision>> {
        self.requests
            .lock()
            .expect("approval requests")
            .push(request.clone());
        Box::pin(async { Ok(Some(ApprovalDecision::allow())) })
    }
}

#[derive(Clone, Default)]
struct RecordingContextProvider {
    agent_names: Arc<Mutex<Vec<String>>>,
}

impl ContextProvider for RecordingContextProvider {
    fn fragments(
        &self,
        request: &ContextRequest<'_>,
    ) -> Result<Vec<ContextFragment>, ContextError> {
        self.agent_names
            .lock()
            .expect("context provider calls")
            .push(request.agent_name.to_string());
        Ok(Vec::new())
    }
}

#[derive(Clone, Default)]
struct RecordingEventStore {
    events: Arc<Mutex<Vec<RunEvent>>>,
}

impl RunEventStore for RecordingEventStore {
    fn append(&self, event: &RunEvent) -> Result<(), EventStoreError> {
        self.events
            .lock()
            .expect("stored events")
            .push(event.clone());
        Ok(())
    }

    fn replay(&self, _query: RunEventReplayQuery) -> Result<RunEventIter, EventStoreError> {
        let events = self.events.lock().expect("stored events").clone();
        Ok(Box::new(events.into_iter().map(Ok)))
    }
}

#[derive(Clone, Default)]
struct RecordingTraceSink {
    ended: Arc<Mutex<Vec<Span>>>,
}

impl TraceSink for RecordingTraceSink {
    fn on_span_start(&self, _span: &Span) {}

    fn on_span_end(&self, span: &Span) {
        self.ended.lock().expect("ended spans").push(span.clone());
    }
}

#[derive(Clone, Default)]
struct RecordingHook {
    calls: Arc<Mutex<Vec<(String, String)>>>,
}

impl RuntimeHook for RecordingHook {
    fn before_tool_call(
        &self,
        event: vv_agent::BeforeToolCallEvent<'_>,
    ) -> Option<vv_agent::BeforeToolCallPatch> {
        let agent_name = event
            .task
            .metadata
            .get("agent_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        self.calls
            .lock()
            .expect("hook calls")
            .push((agent_name, event.call.name.clone()));
        None
    }
}

fn background_agent_with_observer(
    observed: Arc<Mutex<Option<ObservedContext>>>,
    expected_workspace_backend: Arc<dyn WorkspaceBackend>,
    expected_model_provider: Arc<dyn ModelProvider>,
) -> Agent {
    let observer = StaticTool::new(
        "observe_context",
        "Record the child tool context.",
        json!({"type": "object", "properties": {}, "required": []}),
        Arc::new(move |context, _arguments| {
            let thread_workers = match context.execution_backend.as_ref() {
                Some(vv_agent::RuntimeExecutionBackend::Thread(backend)) => {
                    Some(backend.max_workers())
                }
                _ => None,
            };
            let snapshot = ObservedContext {
                workspace: context.workspace.clone(),
                shared_state: context.shared_state.clone(),
                metadata: context.metadata.clone(),
                app_state: context.app_state::<String>().cloned(),
                inherited_workspace_backend: Arc::ptr_eq(
                    &context.workspace_backend,
                    &expected_workspace_backend,
                ),
                inherited_model_provider: context
                    .model_provider
                    .as_ref()
                    .is_some_and(|provider| Arc::ptr_eq(provider, &expected_model_provider)),
                thread_workers,
            };
            *observed.lock().expect("observed context lock") = Some(snapshot);
            ToolOutput::text("observed").to_result(&context.tool_call_id)
        }),
    );

    Agent::builder("background-worker")
        .instructions("Inspect the inherited context, then finish.")
        .model(ModelRef::backend("context", "worker-model"))
        .tool(observer)
        .build()
        .expect("background agent")
}

#[tokio::test]
async fn start_inherits_tool_context_and_merges_explicit_maps() {
    let observed = Arc::new(Mutex::new(None));
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedModelProvider::new(
        "context",
        "worker-model",
        vec![
            LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "observe",
                    "observe_context",
                    json!({}),
                )],
            ),
            finish_response("done"),
        ],
    ));
    let workspace_backend: Arc<dyn WorkspaceBackend> = Arc::new(MemoryWorkspaceBackend::default());
    let agent = background_agent_with_observer(
        observed.clone(),
        workspace_backend.clone(),
        provider.clone(),
    );
    let task = agent.as_background_task().build().expect("background task");
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "runner",
            "unused-model",
            Vec::new(),
        ))
        .workspace("./runner-workspace")
        .build()
        .expect("runner");
    let mut context = ToolContext::new("./context-workspace");
    context.workspace_backend = workspace_backend;
    context.model_provider = Some(provider);
    context.execution_backend = Some(ThreadBackend::new(3).into());
    context.app_state = Some(Arc::new("context-app-state".to_string()));
    context
        .shared_state
        .insert("context_only".to_string(), json!(1));
    context
        .shared_state
        .insert("overridden".to_string(), json!("context"));
    context
        .metadata
        .insert("context_only".to_string(), json!(1));
    context
        .metadata
        .insert("overridden".to_string(), json!("context"));
    context
        .metadata
        .insert("agent_name".to_string(), json!("parent-agent"));
    context
        .metadata
        .insert("_vv_agent_run_id".to_string(), json!("parent-run"));

    let mut override_config = RunConfig::default();
    override_config
        .initial_shared_state
        .insert("override_only".to_string(), json!(2));
    override_config
        .initial_shared_state
        .insert("overridden".to_string(), json!("override"));
    override_config
        .metadata
        .insert("override_only".to_string(), json!(2));
    override_config
        .metadata
        .insert("overridden".to_string(), json!("override"));

    let handle = task
        .start(
            &runner,
            &context,
            json!({"task_description": "inspect inherited context"}),
            Some(override_config),
        )
        .expect("start task");
    let snapshot = handle.wait().await.expect("wait for task");
    assert_eq!(
        snapshot.status(),
        AgentStatus::Completed,
        "background child failed: {:?}",
        snapshot.error()
    );

    let observed = observed
        .lock()
        .expect("observed context lock")
        .clone()
        .expect("captured child context");
    assert_eq!(observed.workspace, PathBuf::from("./context-workspace"));
    assert_eq!(observed.shared_state["context_only"], json!(1));
    assert_eq!(observed.shared_state["override_only"], json!(2));
    assert_eq!(observed.shared_state["overridden"], json!("override"));
    assert_eq!(observed.metadata["context_only"], json!(1));
    assert_eq!(observed.metadata["override_only"], json!(2));
    assert_eq!(observed.metadata["overridden"], json!("override"));
    assert_eq!(observed.metadata["agent_name"], json!("background-worker"));
    assert_ne!(observed.metadata["_vv_agent_run_id"], json!("parent-run"));
    assert_eq!(observed.app_state.as_deref(), Some("context-app-state"));
    assert!(observed.inherited_workspace_backend);
    assert!(observed.inherited_model_provider);
    assert_eq!(observed.thread_workers, Some(3));
}

#[tokio::test]
async fn model_triggered_child_inherits_approval_and_observability_without_run_state() {
    let approved_executions = Arc::new(AtomicUsize::new(0));
    let approved_executions_for_tool = approved_executions.clone();
    let child_observation = Arc::new(Mutex::new(None));
    let child_observation_for_tool = child_observation.clone();
    let approved_tool = FunctionTool::builder("approved_action")
        .needs_approval(true)
        .handler(move |context, _arguments: Value| {
            let approved_executions = approved_executions_for_tool.clone();
            let child_observation = child_observation_for_tool.clone();
            async move {
                approved_executions.fetch_add(1, Ordering::SeqCst);
                *child_observation.lock().expect("child observation") = Some((
                    context.run.agent_name.clone(),
                    context.run.run_id.clone(),
                    context.run.metadata.get("parent_metadata").cloned(),
                    context.app_state::<String>().cloned(),
                    context.shared_state_value("parent_state"),
                ));
                Ok(ToolOutput::text("approved"))
            }
        })
        .build()
        .expect("approval tool");
    let child_agent = Agent::builder("background-policy-child")
        .instructions("Run the approved action, then finish.")
        .model(ModelRef::backend("scripted", "child-model"))
        .tool(approved_tool)
        .build()
        .expect("child agent");
    let task = child_agent
        .as_background_task()
        .name("start_policy_child")
        .build()
        .expect("background task");

    let child_requests = Arc::new(Mutex::new(Vec::<Vec<Message>>::new()));
    let first_child_request = child_requests.clone();
    let second_child_request = child_requests.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "parent-model",
        vec![
            ScriptStep::callback(|request| {
                assert_eq!(request.model, "parent-model");
                Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "start-background",
                        "start_policy_child",
                        json!({"task_description": "exercise inherited capabilities"}),
                    )],
                ))
            }),
            ScriptStep::callback(move |request| {
                assert_eq!(request.model, "child-model");
                first_child_request
                    .lock()
                    .expect("child requests")
                    .push(request.messages.clone());
                Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "approved-call",
                        "approved_action",
                        json!({}),
                    )],
                ))
            }),
            ScriptStep::callback(move |request| {
                assert_eq!(request.model, "child-model");
                second_child_request
                    .lock()
                    .expect("child requests")
                    .push(request.messages.clone());
                Ok(finish_response("child done"))
            }),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let parent_agent = Agent::builder("background-parent")
        .instructions("Start the background child.")
        .model(ModelRef::backend("scripted", "parent-model"))
        .tool(task.clone())
        .tool_use_behavior(ToolUseBehavior::StopOnFirstTool)
        .build()
        .expect("parent agent");

    let approvals = RecordingApprovalProvider::default();
    let contexts = RecordingContextProvider::default();
    let event_store = RecordingEventStore::default();
    let trace_sink = RecordingTraceSink::default();
    let hook = RecordingHook::default();
    let before_cycle_calls = Arc::new(AtomicUsize::new(0));
    let before_cycle_calls_for_config = before_cycle_calls.clone();
    let parent_cancellation = vv_agent::CancellationToken::default();
    let config = RunConfig::builder()
        .session(MemorySession::new("parent-session"))
        .initial_messages(vec![Message::user("parent-only-history")])
        .tool_policy(ToolPolicy {
            approval: ApprovalPolicy::OnRequest,
            ..ToolPolicy::default()
        })
        .approval_provider(Arc::new(approvals.clone()))
        .hook(Arc::new(hook.clone()))
        .trace_sink(Arc::new(trace_sink.clone()))
        .event_store(Arc::new(event_store.clone()))
        .context_provider(Arc::new(contexts.clone()))
        .app_state("parent-app-state".to_string())
        .initial_shared_state(BTreeMap::from([(
            "parent_state".to_string(),
            json!("retained"),
        )]))
        .metadata("parent_metadata", json!("retained"))
        .before_cycle_messages(move |_cycle, _messages, _state| {
            before_cycle_calls_for_config.fetch_add(1, Ordering::SeqCst);
            Vec::new()
        })
        .cancellation_token(parent_cancellation.clone())
        .build();

    let parent_result = runner
        .run_with_config(&parent_agent, "start child", config)
        .await
        .expect("parent run");
    let payload: Value =
        serde_json::from_str(&parent_result.result().cycles[0].tool_results[0].content)
            .expect("background tool output");
    let task_id = payload["task_id"].as_str().expect("background task id");
    let snapshot = task
        .get_handle(task_id)
        .expect("background handle")
        .wait_with_timeout(Duration::from_secs(5))
        .await
        .expect("background child completion");

    assert_eq!(
        snapshot.status(),
        AgentStatus::Completed,
        "background child failed: {:?}",
        snapshot.error()
    );
    assert_eq!(approved_executions.load(Ordering::SeqCst), 1);
    let approval_requests = approvals.requests.lock().expect("approval requests");
    assert_eq!(approval_requests.len(), 1);
    assert_eq!(approval_requests[0].agent_name, "background-policy-child");
    assert_eq!(approval_requests[0].tool_name, "approved_action");

    let observation = child_observation
        .lock()
        .expect("child observation")
        .clone()
        .expect("approved child tool ran");
    assert_eq!(observation.0, "background-policy-child");
    assert_ne!(observation.1, parent_result.run_id());
    assert_eq!(observation.2, Some(json!("retained")));
    assert_eq!(observation.3.as_deref(), Some("parent-app-state"));
    assert_eq!(observation.4, Some(json!("retained")));

    let hook_calls = hook.calls.lock().expect("hook calls");
    assert!(hook_calls.contains(&(
        "background-policy-child".to_string(),
        "approved_action".to_string(),
    )));
    let context_agents = contexts.agent_names.lock().expect("context provider calls");
    assert!(context_agents.contains(&"background-parent".to_string()));
    assert!(context_agents.contains(&"background-policy-child".to_string()));

    let stored_events = event_store.events.lock().expect("stored events");
    let child_events = stored_events
        .iter()
        .filter(|event| event.agent_name() == Some("background-policy-child"))
        .collect::<Vec<_>>();
    assert!(!child_events.is_empty());
    assert!(child_events
        .iter()
        .all(|event| event.session_id().is_none()));
    assert!(child_events
        .iter()
        .all(|event| event.run_id() != parent_result.run_id()));

    let ended_spans = trace_sink.ended.lock().expect("ended spans");
    assert!(ended_spans.iter().any(|span| {
        span.name == "run"
            && span.metadata.get("agent_name") == Some(&json!("background-policy-child"))
            && span.metadata.get("run_id") != Some(&json!(parent_result.run_id()))
    }));
    assert!(child_requests
        .lock()
        .expect("child requests")
        .iter()
        .flatten()
        .all(|message| !message.content.contains("parent-only-history")));
    assert_eq!(before_cycle_calls.load(Ordering::SeqCst), 1);
    assert!(!parent_cancellation.is_cancelled());
}

#[tokio::test]
async fn model_triggered_child_cannot_bypass_parent_tool_policy() {
    let blocked_executions = Arc::new(AtomicUsize::new(0));
    let blocked_executions_for_tool = blocked_executions.clone();
    let blocked_tool = StaticTool::new(
        "blocked_action",
        "This action is denied by the parent run policy.",
        json!({"type": "object", "properties": {}, "required": []}),
        Arc::new(move |context, _arguments| {
            blocked_executions_for_tool.fetch_add(1, Ordering::SeqCst);
            ToolOutput::text("must not run").to_result(&context.tool_call_id)
        }),
    );
    let child_agent = Agent::builder("background-policy-child")
        .instructions("Attempt the blocked action.")
        .model(ModelRef::backend("scripted", "policy-child-model"))
        .tool(blocked_tool)
        .build()
        .expect("policy child agent");
    let task = child_agent
        .as_background_task()
        .name("start_blocked_child")
        .build()
        .expect("background task");
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "policy-parent-model",
        vec![
            ScriptStep::callback(|request| {
                assert_eq!(request.model, "policy-parent-model");
                Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "start-blocked",
                        "start_blocked_child",
                        json!({"task_description": "attempt blocked action"}),
                    )],
                ))
            }),
            ScriptStep::callback(|request| {
                assert_eq!(request.model, "policy-child-model");
                Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "blocked-call",
                        "blocked_action",
                        json!({}),
                    )],
                ))
            }),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let parent_agent = Agent::builder("policy-parent")
        .instructions("Start the background child.")
        .model(ModelRef::backend("scripted", "policy-parent-model"))
        .tool(task.clone())
        .tool_use_behavior(ToolUseBehavior::StopOnFirstTool)
        .build()
        .expect("parent agent");
    let parent_result = runner
        .run_with_config(
            &parent_agent,
            "start child",
            RunConfig::builder()
                .tool_policy(ToolPolicy::default().disallow("blocked_action"))
                .build(),
        )
        .await
        .expect("parent run");
    let payload: Value =
        serde_json::from_str(&parent_result.result().cycles[0].tool_results[0].content)
            .expect("background tool output");
    let task_id = payload["task_id"].as_str().expect("background task id");
    let snapshot = task
        .get_handle(task_id)
        .expect("background handle")
        .wait_with_timeout(Duration::from_secs(5))
        .await
        .expect("background child completion");

    assert_eq!(snapshot.status(), AgentStatus::Failed);
    assert_eq!(blocked_executions.load(Ordering::SeqCst), 0);
    assert!(snapshot.error().is_some());
}

#[tokio::test]
async fn registry_retrieves_concurrently_running_handles() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedModelProvider::new(
        "context",
        "worker-model",
        vec![finish_response("first"), finish_response("second")],
    ));
    let runner = Runner::builder()
        .model_provider_arc(provider.clone())
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("worker")
        .instructions("Finish the task.")
        .model(ModelRef::backend("context", "worker-model"))
        .build()
        .expect("agent");
    let task = agent.as_background_task().build().expect("background task");
    let mut context = ToolContext::new("./workspace");
    context.model_provider = Some(provider);

    let first = task
        .start(
            &runner,
            &context,
            json!({"task_description": "first"}),
            None,
        )
        .expect("first task");
    let second = task
        .start(
            &runner,
            &context,
            json!({"task_description": "second"}),
            None,
        )
        .expect("second task");

    assert_ne!(first.task_id(), second.task_id());
    assert_eq!(
        task.get_handle(first.task_id())
            .expect("retrieve first")
            .task_id(),
        first.task_id()
    );
    assert_eq!(
        task.get_handle(second.task_id())
            .expect("retrieve second")
            .task_id(),
        second.task_id()
    );
    assert!(task.get_handle("missing-task").is_err());
    assert_eq!(
        first.wait().await.expect("first result").status(),
        AgentStatus::Completed
    );
    assert_eq!(
        second.wait().await.expect("second result").status(),
        AgentStatus::Completed
    );
}

#[tokio::test]
async fn model_tool_handler_returns_a_retrievable_task_id() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedModelProvider::new(
        "context",
        "worker-model",
        vec![finish_response("tool-started")],
    ));
    let agent = Agent::builder("tool-worker")
        .instructions("Finish the task.")
        .model(ModelRef::backend("context", "worker-model"))
        .build()
        .expect("agent");
    let task = agent
        .as_background_task()
        .name("start_tool_worker")
        .build()
        .expect("background task");
    let spec = task.as_tool_spec();
    let mut context = ToolContext::new("./workspace");
    context.model_provider = Some(provider);
    context.begin_tool_call(&ToolCall::from_raw_arguments(
        "background-call",
        "start_tool_worker",
        json!({"task_description": "run from a model tool call"}),
    ));
    let arguments = BTreeMap::from([(
        "task_description".to_string(),
        json!("run from a model tool call"),
    )]);

    let result = (spec.handler)(&mut context, &arguments);
    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(result.tool_call_id, "background-call");
    let payload: Value = serde_json::from_str(&result.content).expect("JSON tool output");
    assert_eq!(payload["status"], json!("background_task_started"));
    let task_id = payload["task_id"].as_str().expect("task id");
    let handle = task.get_handle(task_id).expect("retrieve task handle");
    assert_eq!(handle.task_id(), task_id);
    assert_eq!(
        handle.wait().await.expect("background result").status(),
        AgentStatus::Completed
    );
}

#[tokio::test]
async fn worker_panic_transitions_to_failed_without_poisoning_state() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedModelProvider::from_callback(
        "panic",
        "panic-model",
        |_request| panic!("provider panic payload must not escape"),
    ));
    let runner = Runner::builder()
        .model_provider_arc(provider.clone())
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("panic-worker")
        .instructions("Trigger the provider.")
        .model(ModelRef::backend("panic", "panic-model"))
        .build()
        .expect("agent");
    let task = agent.as_background_task().build().expect("background task");
    let mut context = ToolContext::new("./workspace");
    context.model_provider = Some(provider);

    let handle = task
        .start(
            &runner,
            &context,
            json!({"task_description": "panic in the worker"}),
            None,
        )
        .expect("start task");
    let snapshot = handle
        .wait_with_timeout(Duration::from_secs(2))
        .await
        .expect("panic must make the task terminal");

    assert_eq!(snapshot.status(), AgentStatus::Failed);
    assert_eq!(
        snapshot.error(),
        Some("background agent task worker panicked")
    );
    assert_eq!(handle.status(), AgentStatus::Failed);
    assert_eq!(
        handle.poll().expect("state remains readable after panic"),
        snapshot
    );
}

#[tokio::test]
async fn failed_run_result_projects_raw_error_into_snapshot() {
    let provider: Arc<dyn ModelProvider> = Arc::new(ScriptedModelProvider::new(
        "guardrail",
        "unused-model",
        Vec::new(),
    ));
    let runner = Runner::builder()
        .model_provider_arc(provider.clone())
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("guarded-worker")
        .instructions("This run is rejected before model resolution.")
        .input_guardrail(Arc::new(RejectBackgroundInput))
        .build()
        .expect("agent");
    let task = agent.as_background_task().build().expect("background task");
    let mut context = ToolContext::new("./workspace");
    context.model_provider = Some(provider);

    let snapshot = task
        .start(
            &runner,
            &context,
            json!({"task_description": "reject this input"}),
            None,
        )
        .expect("start task")
        .wait_with_timeout(Duration::from_secs(2))
        .await
        .expect("failed result must be terminal");

    assert_eq!(snapshot.status(), AgentStatus::Failed);
    assert_eq!(snapshot.error(), Some("background input rejected"));
}
