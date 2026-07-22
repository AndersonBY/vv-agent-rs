use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde_json::json;
use vv_agent::types::AgentTask;
use vv_agent::{
    Agent, AgentResult, AgentRuntime, AgentStatus, CancellationToken, CompletionReason,
    ExecutionContext, GuardrailOutcome, LLMResponse, LlmClient, LlmError, LlmRequest, ModelRef,
    OutputGuardrail, RunConfig, RunContext, RunEventPayload, RunHandleStatus, Runner,
    RuntimeExecutionBackend, RuntimeRunControls, ScriptedModelProvider, SubAgentConfig,
    SubTaskManager, ThreadBackend, ToolCall, ToolResultStatus,
};

#[derive(Clone)]
struct BlockingChildClient {
    calls: Arc<Mutex<usize>>,
    child_started: std::sync::mpsc::Sender<()>,
    release_child: Arc<(Mutex<bool>, Condvar)>,
}

impl LlmClient for BlockingChildClient {
    fn complete(&self, _request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let call = {
            let mut calls = self.calls.lock().expect("call count");
            *calls += 1;
            *calls
        };
        match call {
            1 => Ok(LLMResponse::with_tool_calls(
                "delegate",
                vec![ToolCall::new(
                    "parent_sub_call",
                    "create_sub_task",
                    BTreeMap::from([
                        ("agent_id".to_string(), json!("researcher")),
                        (
                            "task_description".to_string(),
                            json!("inspect cancellation"),
                        ),
                    ]),
                )],
            )),
            2 => {
                self.child_started.send(()).expect("signal child start");
                let (released, wake) = &*self.release_child;
                let mut released = released.lock().expect("release lock");
                while !*released {
                    released = wake.wait(released).expect("release wait");
                }
                Ok(finish_response("child completed"))
            }
            _ => Err(LlmError::Request(
                "parent should stop after cancellation".to_string(),
            )),
        }
    }
}

#[test]
fn parent_cancellation_is_derived_by_runtime_sub_agent_session() {
    let (child_started_tx, child_started_rx) = std::sync::mpsc::channel();
    let release_child = Arc::new((Mutex::new(false), Condvar::new()));
    let client = BlockingChildClient {
        calls: Arc::new(Mutex::new(0)),
        child_started: child_started_tx,
        release_child: release_child.clone(),
    };
    let token = CancellationToken::default();
    let token_for_run = token.clone();
    let mut task = AgentTask::new("parent-cancel", "demo", "parent system", "delegate");
    task.sub_agents.insert(
        "researcher".to_string(),
        SubAgentConfig::new("demo", "research profile"),
    );

    let run = std::thread::spawn(move || {
        AgentRuntime::new(client)
            .run_with_controls(
                task,
                RuntimeRunControls {
                    execution_context: Some(
                        ExecutionContext::default().with_cancellation_token(token_for_run),
                    ),
                    ..RuntimeRunControls::default()
                },
            )
            .expect("runtime result")
    });
    child_started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("child LLM started");
    token.cancel();
    {
        let (released, wake) = &*release_child;
        *released.lock().expect("release lock") = true;
        wake.notify_all();
    }

    let result = run.join().expect("runtime thread");
    assert_eq!(result.status, AgentStatus::Failed);
    assert!(result
        .error
        .as_deref()
        .is_some_and(|error| error.to_ascii_lowercase().contains("cancel")));
    let sub_task_result = result
        .cycles
        .iter()
        .flat_map(|cycle| &cycle.tool_results)
        .find(|result| result.tool_call_id == "parent_sub_call")
        .expect("sub-task result");
    assert_eq!(sub_task_result.status, ToolResultStatus::Error);
    let child: serde_json::Value =
        serde_json::from_str(&sub_task_result.content).expect("child outcome");
    assert_eq!(child["status"], "failed");
    assert!(child["error"]
        .as_str()
        .is_some_and(|error| error.to_ascii_lowercase().contains("cancel")));
}

#[derive(Clone)]
struct BlockingConfiguredSubAgentClient {
    parent_response: LLMResponse,
    parent_calls: Arc<Mutex<usize>>,
    child_started: std::sync::mpsc::Sender<()>,
    release: Arc<(Mutex<bool>, Condvar)>,
}

impl BlockingConfiguredSubAgentClient {
    fn wait_for_release(&self) {
        let (released, wake) = &*self.release;
        let mut released = released.lock().expect("release lock");
        while !*released {
            released = wake.wait(released).expect("release wait");
        }
    }
}

impl LlmClient for BlockingConfiguredSubAgentClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let is_child_request = request
            .messages
            .first()
            .is_some_and(|message| message.content.contains("research profile"));
        if is_child_request {
            self.child_started.send(()).expect("signal child start");
            self.wait_for_release();
            return Ok(finish_response("child completed"));
        }

        let parent_call = {
            let mut parent_calls = self.parent_calls.lock().expect("parent call count");
            *parent_calls += 1;
            *parent_calls
        };
        if parent_call == 1 {
            return Ok(self.parent_response.clone());
        }

        self.wait_for_release();
        Ok(finish_response("parent should be cancelled"))
    }
}

#[test]
fn parent_cancellation_reaches_async_configured_sub_agent_thread() {
    let arguments = BTreeMap::from([
        ("agent_id".to_string(), json!("researcher")),
        (
            "task_description".to_string(),
            json!("inspect async cancellation"),
        ),
        ("wait_for_completion".to_string(), json!(false)),
    ]);

    let (result, manager) = run_with_blocked_configured_children(arguments, 1, None);
    let create_result = find_tool_result(&result, "parent_sub_call");
    let payload: serde_json::Value =
        serde_json::from_str(&create_result.content).expect("async create payload");
    let task_id = payload["task_id"].as_str().expect("async task id");
    let snapshot = manager
        .wait_for_record(task_id, Some(Duration::from_secs(2)))
        .expect("async child record");
    let outcome = snapshot.outcome.expect("async child outcome");

    assert_eq!(outcome.status, AgentStatus::Failed);
    assert!(outcome
        .error
        .as_deref()
        .is_some_and(|error| error.to_ascii_lowercase().contains("cancel")));
}

#[test]
fn parent_cancellation_reaches_configured_sub_agent_batch_workers() {
    let arguments = BTreeMap::from([
        ("agent_id".to_string(), json!("researcher")),
        (
            "tasks".to_string(),
            json!([
                {"task_description": "inspect batch cancellation A"},
                {"task_description": "inspect batch cancellation B"}
            ]),
        ),
    ]);
    let backend = RuntimeExecutionBackend::Thread(ThreadBackend::new(2));

    let (result, _) = run_with_blocked_configured_children(arguments, 2, Some(backend));
    let create_result = find_tool_result(&result, "parent_sub_call");
    assert_eq!(create_result.status, ToolResultStatus::Error);
    let payload: serde_json::Value =
        serde_json::from_str(&create_result.content).expect("batch create payload");
    let results = payload["details"]["results"]
        .as_array()
        .expect("batch child results");

    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|child| {
        child["status"] == "failed"
            && child["error"]
                .as_str()
                .is_some_and(|error| error.to_ascii_lowercase().contains("cancel"))
    }));
}

fn run_with_blocked_configured_children(
    arguments: BTreeMap<String, serde_json::Value>,
    expected_child_starts: usize,
    execution_backend: Option<RuntimeExecutionBackend>,
) -> (AgentResult, SubTaskManager) {
    let (child_started_tx, child_started_rx) = std::sync::mpsc::channel();
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let client = BlockingConfiguredSubAgentClient {
        parent_response: LLMResponse::with_tool_calls(
            "delegate",
            vec![ToolCall::new(
                "parent_sub_call",
                "create_sub_task",
                arguments,
            )],
        ),
        parent_calls: Arc::new(Mutex::new(0)),
        child_started: child_started_tx,
        release: release.clone(),
    };
    let token = CancellationToken::default();
    let token_for_run = token.clone();
    let manager = SubTaskManager::default();
    let manager_for_run = manager.clone();
    let mut task = AgentTask::new("parent-cancel", "demo", "parent system", "delegate");
    task.sub_agents.insert(
        "researcher".to_string(),
        SubAgentConfig::new("demo", "research profile"),
    );

    let run = std::thread::spawn(move || {
        let mut runtime = AgentRuntime::new(client);
        if let Some(execution_backend) = execution_backend {
            runtime = runtime.with_execution_backend(execution_backend);
        }
        runtime
            .run_with_controls(
                task,
                RuntimeRunControls {
                    execution_context: Some(
                        ExecutionContext::default().with_cancellation_token(token_for_run),
                    ),
                    sub_task_manager: Some(manager_for_run),
                    ..RuntimeRunControls::default()
                },
            )
            .expect("runtime result")
    });

    for _ in 0..expected_child_starts {
        if let Err(error) = child_started_rx.recv_timeout(Duration::from_secs(2)) {
            release_waiters(&release);
            let _ = run.join();
            panic!("configured child LLM did not start: {error}");
        }
    }
    token.cancel();
    release_waiters(&release);

    (run.join().expect("runtime thread"), manager)
}

fn release_waiters(release: &Arc<(Mutex<bool>, Condvar)>) {
    let (released, wake) = &**release;
    *released.lock().expect("release lock") = true;
    wake.notify_all();
}

fn find_tool_result<'a>(
    result: &'a AgentResult,
    tool_call_id: &str,
) -> &'a vv_agent::ToolExecutionResult {
    result
        .cycles
        .iter()
        .flat_map(|cycle| &cycle.tool_results)
        .find(|tool_result| tool_result.tool_call_id == tool_call_id)
        .expect("tool result")
}

#[derive(Clone)]
struct CancelAfterRuntime(CancellationToken);

impl OutputGuardrail for CancelAfterRuntime {
    fn check(&self, _ctx: &RunContext, output: &AgentResult) -> GuardrailOutcome<AgentResult> {
        self.0.cancel();
        GuardrailOutcome::Allow(output.clone())
    }
}

#[derive(Clone)]
struct CancelAndBlock(CancellationToken);

impl OutputGuardrail for CancelAndBlock {
    fn check(&self, _ctx: &RunContext, _output: &AgentResult) -> GuardrailOutcome<AgentResult> {
        self.0.cancel();
        GuardrailOutcome::Block {
            message: "guardrail blocked while cancelling".to_string(),
        }
    }
}

#[derive(Clone)]
struct SideEffectBlock(Arc<Mutex<usize>>);

impl OutputGuardrail for SideEffectBlock {
    fn check(&self, _ctx: &RunContext, _output: &AgentResult) -> GuardrailOutcome<AgentResult> {
        *self.0.lock().expect("guardrail calls") += 1;
        GuardrailOutcome::Block {
            message: "guardrail must not replace cancellation".to_string(),
        }
    }
}

#[tokio::test]
async fn cancelled_result_skips_output_guardrails_and_preserves_cancellation_error() {
    let token = CancellationToken::default();
    token.cancel_with_reason("cancelled by caller");
    let guardrail_calls = Arc::new(Mutex::new(0));
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "cancel-model",
            vec![finish_response("must not run")],
        ))
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("pre-cancelled-agent")
        .instructions("Do not run after cancellation.")
        .model(ModelRef::named("cancel-model"))
        .output_guardrail(Arc::new(SideEffectBlock(guardrail_calls.clone())))
        .build()
        .expect("agent");

    let result = runner
        .run_with_config(
            &agent,
            "finish",
            RunConfig::builder().cancellation_token(token).build(),
        )
        .await
        .expect("cancelled result");

    assert_eq!(*guardrail_calls.lock().expect("guardrail calls"), 0);
    assert_eq!(result.status(), AgentStatus::Failed);
    assert_eq!(
        result.completion_reason(),
        Some(CompletionReason::Cancelled)
    );
    assert_eq!(result.final_output(), Some("cancelled by caller"));
    assert_eq!(
        result.result().error.as_deref(),
        Some("cancelled by caller")
    );
    let terminal = result.events().last().expect("terminal event");
    assert!(matches!(
        terminal.payload(),
        RunEventPayload::RunCancelled { .. }
    ));
    assert_eq!(
        terminal.completion_reason(),
        Some(CompletionReason::Cancelled)
    );
}

#[tokio::test]
async fn cancellation_precedes_output_guardrail_failure_in_result_and_event() {
    let token = CancellationToken::default();
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "cancel-model",
            vec![finish_response("tool final output")],
        ))
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("cancel-guardrail-agent")
        .instructions("Finish immediately.")
        .model(ModelRef::named("cancel-model"))
        .output_guardrail(Arc::new(CancelAndBlock(token.clone())))
        .build()
        .expect("agent");

    let result = runner
        .run_with_config(
            &agent,
            "finish",
            RunConfig::builder().cancellation_token(token).build(),
        )
        .await
        .expect("cancelled result");

    assert_eq!(result.status(), AgentStatus::Failed);
    assert_eq!(
        result.completion_reason(),
        Some(CompletionReason::Cancelled)
    );
    assert_eq!(result.partial_output(), Some("finished"));
    assert_eq!(result.final_output(), Some("Operation was cancelled"));
    let terminal = result.events().last().expect("terminal event");
    assert!(matches!(
        terminal.payload(),
        RunEventPayload::RunCancelled { .. }
    ));
    assert_eq!(
        terminal.completion_reason(),
        Some(CompletionReason::Cancelled)
    );
    assert_eq!(terminal.partial_output(), result.partial_output());
}

#[tokio::test]
async fn late_and_repeated_cancel_do_not_replace_completed_handle_state() {
    let token = CancellationToken::default();
    let provider = ScriptedModelProvider::new(
        "scripted",
        "cancel-model",
        vec![finish_response("completed result")],
    );
    let workspace = tempfile::tempdir().expect("workspace");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(workspace.path())
        .build()
        .expect("runner");
    let agent = Agent::builder("late-cancel-agent")
        .instructions("Finish immediately.")
        .model(ModelRef::named("cancel-model"))
        .output_guardrail(Arc::new(CancelAfterRuntime(token.clone())))
        .build()
        .expect("agent");
    let handle = runner
        .start(
            &agent,
            "finish",
            RunConfig::builder().cancellation_token(token).build(),
        )
        .await
        .expect("start");

    let result = handle.result().await.expect("completed result");
    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(handle.state().status, RunHandleStatus::Completed);

    assert!(!handle.cancel());
    assert!(!handle.cancel());
    assert_eq!(handle.state().status, RunHandleStatus::Completed);
    assert!(!handle.state().cancelled);
}

fn finish_response(message: &str) -> LLMResponse {
    LLMResponse::with_tool_calls(
        "finished",
        vec![ToolCall::new(
            "finish",
            "task_finish",
            BTreeMap::from([("message".to_string(), json!(message))]),
        )],
    )
}
