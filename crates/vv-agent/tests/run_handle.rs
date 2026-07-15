use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde_json::json;
use sha2::{Digest, Sha256};
use vv_agent::{
    Agent, AgentStatus, ApprovalPolicy, FunctionTool, GuardrailOutcome, InputGuardrail,
    LLMResponse, LlmClient, LlmError, LlmRequest, LlmStreamCallback, ModelError, ModelProvider,
    ModelRef, NormalizedInput, ResolvedModelConfig, RunConfig, RunContext, RunEventPayload,
    RunHandleState, RunHandleStatus, Runner, ScriptStep, ScriptedModelProvider, ToolCall,
    ToolOutput, ToolPolicy,
};

const RUN_HANDLE_FIXTURE: &str = include_str!("fixtures/parity/run_handle_v1.json");
const RUN_HANDLE_FIXTURE_SHA256: &str =
    "aa6d933f26674beeb68964fa320e62859711114bdd7581ebde1b281f18a439bf";

fn run_handle_contract() -> serde_json::Value {
    assert_eq!(
        format!("{:x}", Sha256::digest(RUN_HANDLE_FIXTURE.as_bytes())),
        RUN_HANDLE_FIXTURE_SHA256
    );
    serde_json::from_str(RUN_HANDLE_FIXTURE).expect("run handle fixture")
}

#[tokio::test]
async fn runner_start_yields_tool_started_before_result_is_ready() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let ran = Arc::new(Mutex::new(false));
    let gate_for_tool = gate.clone();
    let ran_for_tool = ran.clone();
    let slow_tool = FunctionTool::builder("slow_tool")
        .description("Wait until test releases the gate.")
        .json_schema(json!({"type":"object","properties":{},"required":[]}))
        .handler(move |_ctx, _args: serde_json::Value| {
            let gate = gate_for_tool.clone();
            let ran = ran_for_tool.clone();
            async move {
                gate.notified().await;
                *ran.lock().expect("lock") = true;
                Ok(ToolOutput::text("slow done"))
            }
        })
        .build()
        .expect("tool");

    let provider = ScriptedModelProvider::new(
        "scripted",
        "demo-model",
        vec![
            LLMResponse::with_tool_calls(
                "calling",
                vec![ToolCall::from_raw_arguments(
                    "call_1",
                    "slow_tool",
                    json!({}),
                )],
            ),
            LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "finish",
                    "task_finish",
                    json!({"message":"done"}),
                )],
            ),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Call slow_tool, then finish.")
        .model(ModelRef::named("demo-model"))
        .tool(slow_tool)
        .build()
        .expect("agent");

    let handle = runner
        .start(&agent, "go", RunConfig::default())
        .await
        .expect("start");
    let mut events = handle.events();
    let mut saw_started = false;
    while let Some(event) = tokio::time::timeout(Duration::from_secs(2), events.next())
        .await
        .expect("event timeout")
    {
        let event = event.expect("event");
        if matches!(event.payload(), RunEventPayload::ToolCallStarted { tool_name, .. } if tool_name == "slow_tool")
        {
            assert!(!handle.state().done);
            saw_started = true;
            gate.notify_one();
            break;
        }
    }

    assert!(saw_started);
    let result = handle.result().await.expect("result");
    assert_eq!(result.final_output(), Some("done"));
    assert_eq!(
        handle
            .result()
            .await
            .expect("repeatable result")
            .final_output(),
        Some("done")
    );
    assert_eq!(handle.state().status, RunHandleStatus::Completed);
    assert!(*ran.lock().expect("lock"));
}

#[test]
fn run_handle_state_preserves_non_success_terminal_statuses() {
    assert_eq!(
        RunHandleState::from_agent_status(AgentStatus::WaitUser).status,
        RunHandleStatus::WaitUser
    );
    assert_eq!(
        RunHandleState::from_agent_status(AgentStatus::MaxCycles).status,
        RunHandleStatus::MaxCycles
    );
}

#[tokio::test]
async fn ordinary_run_handle_rejects_interactive_control_facades() {
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "demo-model",
            vec![LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "finish",
                    "task_finish",
                    json!({"message": "done"}),
                )],
            )],
        ))
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Finish.")
        .model(ModelRef::named("demo-model"))
        .build()
        .expect("agent");
    let handle = runner
        .start(&agent, "go", RunConfig::default())
        .await
        .expect("start");

    assert_eq!(
        handle.steer("change direction").expect_err("unsupported"),
        "RunHandle.steer() is only available when the handle is attached to an interactive session."
    );
    assert_eq!(
        handle.follow_up("continue").expect_err("unsupported"),
        "RunHandle.follow_up() is only available when the handle is attached to an interactive session."
    );
    assert_eq!(
        handle.result().await.expect("result").final_output(),
        Some("done")
    );
}

struct RejectInput;

impl InputGuardrail for RejectInput {
    fn check(
        &self,
        _context: &RunContext,
        _input: &NormalizedInput,
    ) -> GuardrailOutcome<NormalizedInput> {
        GuardrailOutcome::Block {
            message: "input rejected by regression guardrail".to_string(),
        }
    }
}

#[tokio::test]
async fn run_handle_failed_state_preserves_real_runner_error() {
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "demo-model",
            Vec::new(),
        ))
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Finish.")
        .model(ModelRef::named("demo-model"))
        .input_guardrail(Arc::new(RejectInput))
        .build()
        .expect("agent");
    let handle = runner
        .start(&agent, "go", RunConfig::default())
        .await
        .expect("start");

    let result = handle.result().await.expect("failed run result");
    let result_error = result.result().error.clone();
    let state = handle.state();

    assert_eq!(result.status(), AgentStatus::Failed);
    assert_eq!(
        result_error.as_deref(),
        Some("input rejected by regression guardrail")
    );
    assert_eq!(state.status, RunHandleStatus::Failed);
    assert!(state.done);
    assert!(!state.cancelled);
    assert_eq!(state.error, result_error);
}

#[tokio::test]
async fn wait_user_stream_finishes_when_resume_snapshot_retains_event_callbacks() {
    let dangerous = FunctionTool::builder("dangerous")
        .description("Require manual approval.")
        .json_schema(json!({"type":"object","properties":{},"required":[]}))
        .handler(|_ctx, _args: serde_json::Value| async { Ok(ToolOutput::text("ran")) })
        .build()
        .expect("tool");
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "demo-model",
            vec![LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "call_1",
                    "dangerous",
                    json!({}),
                )],
            )],
        ))
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Use the tool.")
        .model(ModelRef::named("demo-model"))
        .tool(dangerous)
        .tool_policy(ToolPolicy {
            approval: ApprovalPolicy::Always,
            ..ToolPolicy::default()
        })
        .build()
        .expect("agent");
    let mut stream = runner.stream(&agent, "go").await.expect("stream");

    let events = tokio::time::timeout(Duration::from_secs(2), async {
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.expect("event"));
        }
        events
    })
    .await
    .expect("stream completion timeout");
    let result = stream.into_result().await.expect("result");

    assert_eq!(result.status(), AgentStatus::WaitUser);
    assert!(events.iter().any(|event| matches!(
        event.payload(),
        RunEventPayload::ApprovalRequested { tool_name, .. } if tool_name == "dangerous"
    )));
}

#[tokio::test]
async fn run_handle_resume_uses_the_interrupted_runs_origin_context() {
    let executions = Arc::new(AtomicUsize::new(0));
    let executions_for_tool = executions.clone();
    let dangerous = FunctionTool::builder("dangerous")
        .description("Require manual approval.")
        .json_schema(json!({"type":"object","properties":{},"required":[]}))
        .needs_approval(true)
        .handler(move |_ctx, _args: serde_json::Value| {
            executions_for_tool.fetch_add(1, Ordering::SeqCst);
            async { Ok(ToolOutput::text("ran from origin")) }
        })
        .build()
        .expect("tool");
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "demo-model",
            vec![
                LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "call_1",
                        "dangerous",
                        json!({}),
                    )],
                ),
                LLMResponse::with_tool_calls(
                    "finish",
                    vec![ToolCall::from_raw_arguments(
                        "finish_approved",
                        "task_finish",
                        json!({"message": "ran from origin"}),
                    )],
                ),
            ],
        ))
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Use the tool.")
        .model(ModelRef::named("demo-model"))
        .tool(dangerous)
        .tool_policy(ToolPolicy {
            approval: ApprovalPolicy::OnRequest,
            ..ToolPolicy::default()
        })
        .build()
        .expect("agent");
    let handle = runner
        .start(&agent, "go", RunConfig::default())
        .await
        .expect("start");
    let interrupted = handle.result().await.expect("interrupted result");
    let interruption_id = interrupted
        .approvals()
        .first()
        .expect("approval")
        .interruption_id
        .clone();
    let mut state = interrupted.into_state().expect("run state");
    state.approve(&interruption_id).expect("approve");

    let resumed = handle.resume(state).await.expect("resume");

    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_eq!(resumed.final_output(), Some("ran from origin"));
    assert_eq!(executions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn run_handle_resume_with_input_restores_state_and_appends_the_new_input() {
    let requests = Arc::new(Mutex::new(Vec::<LlmRequest>::new()));
    let captured_requests = requests.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "resume-model",
        vec![
            ScriptStep::from(LLMResponse::with_tool_calls(
                "need input",
                vec![ToolCall::from_raw_arguments(
                    "ask_1",
                    "ask_user",
                    json!({"question": "Which color?"}),
                )],
            )),
            ScriptStep::callback(move |request| {
                captured_requests
                    .lock()
                    .expect("requests")
                    .push(request.clone());
                Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "finish_1",
                        "task_finish",
                        json!({"message": "selected blue"}),
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
    let agent = Agent::builder("assistant")
        .instructions("Ask once, then finish.")
        .model(ModelRef::named("resume-model"))
        .build()
        .expect("agent");
    let handle = runner
        .start(&agent, "choose", RunConfig::default())
        .await
        .expect("start");
    let state = handle
        .result()
        .await
        .expect("interrupted result")
        .into_state()
        .expect("run state");

    let resumed = handle
        .resume_with_input(state, "blue")
        .await
        .expect("resume with input");

    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_eq!(resumed.final_output(), Some("selected blue"));
    assert_eq!(
        requests
            .lock()
            .expect("requests")
            .last()
            .expect("resume request")
            .messages
            .last()
            .expect("resume input")
            .content,
        "blue"
    );
}

#[derive(Clone)]
struct BurstStreamingClient {
    gate: Arc<(Mutex<bool>, Condvar)>,
    event_count: usize,
}

impl LlmClient for BurstStreamingClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        self.complete_with_stream(request, None)
    }

    fn complete_with_stream(
        &self,
        _request: LlmRequest,
        stream_callback: Option<LlmStreamCallback>,
    ) -> Result<LLMResponse, LlmError> {
        let (released, wake) = &*self.gate;
        let mut released = released.lock().expect("burst gate");
        while !*released {
            released = wake.wait(released).expect("burst wait");
        }
        drop(released);
        let callback = stream_callback.expect("stream callback");
        for index in 0..self.event_count {
            callback(&BTreeMap::from([
                ("event".to_string(), json!("assistant_delta")),
                ("content_delta".to_string(), json!(index.to_string())),
            ]));
        }
        Ok(LLMResponse::with_tool_calls(
            "done",
            vec![ToolCall::from_raw_arguments(
                "finish",
                "task_finish",
                json!({"message": "done"}),
            )],
        ))
    }
}

#[derive(Clone)]
struct BurstStreamingProvider(BurstStreamingClient);

impl ModelProvider for BurstStreamingProvider {
    fn resolve(&self, model: &ModelRef) -> Result<ResolvedModelConfig, ModelError> {
        Ok(ResolvedModelConfig::new(
            "burst",
            model.model(),
            model.model(),
            model.model(),
            Vec::new(),
        ))
    }

    fn client(&self, _resolved: &ResolvedModelConfig) -> Result<Arc<dyn LlmClient>, ModelError> {
        Ok(Arc::new(self.0.clone()))
    }
}

#[tokio::test]
async fn run_handle_subscribers_are_independent_and_lossless_after_live_capacity() {
    let contract = run_handle_contract();
    let event_count = contract["subscribers"]["burst_event_count"]
        .as_u64()
        .expect("burst event count") as usize;
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let runner = Runner::builder()
        .model_provider(BurstStreamingProvider(BurstStreamingClient {
            gate: gate.clone(),
            event_count,
        }))
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("burst")
        .instructions("Finish.")
        .model(ModelRef::named("burst-model"))
        .build()
        .expect("agent");
    let handle = runner
        .start(&agent, "go", RunConfig::default())
        .await
        .expect("start");
    let mut first = handle.events();
    let mut second = handle.events();
    let (released, wake) = &*gate;
    *released.lock().expect("release burst") = true;
    wake.notify_all();
    let result = handle.result().await.expect("result");

    let mut first_events = Vec::new();
    while let Some(event) = first.next().await {
        first_events.push(event.expect("first event"));
    }
    let mut second_events = Vec::new();
    while let Some(event) = second.next().await {
        second_events.push(event.expect("second event"));
    }

    assert_eq!(contract["subscribers"]["independent"], true);
    assert_eq!(contract["subscribers"]["start_from_complete_backlog"], true);
    assert_eq!(
        contract["subscribers"]["lossless_after_live_capacity"],
        true
    );
    let first_ids = first_events
        .iter()
        .map(|event| event.event_id().as_str())
        .collect::<Vec<_>>();
    let second_ids = second_events
        .iter()
        .map(|event| event.event_id().as_str())
        .collect::<Vec<_>>();
    let result_ids = result
        .events()
        .iter()
        .map(|event| event.event_id().as_str())
        .collect::<Vec<_>>();
    assert_eq!(first_ids, second_ids);
    assert_eq!(first_ids, result_ids);
    assert_eq!(
        first_events
            .iter()
            .filter(|event| matches!(event.payload(), RunEventPayload::AssistantDelta { .. }))
            .count(),
        event_count
    );
}

#[derive(Clone)]
struct BlockingCancellationClient {
    started: Arc<(Mutex<bool>, Condvar)>,
    release: Arc<(Mutex<bool>, Condvar)>,
}

impl LlmClient for BlockingCancellationClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        self.complete_with_stream(request, None)
    }

    fn complete_with_stream(
        &self,
        _request: LlmRequest,
        _stream_callback: Option<LlmStreamCallback>,
    ) -> Result<LLMResponse, LlmError> {
        let (started, started_wake) = &*self.started;
        *started.lock().expect("started lock") = true;
        started_wake.notify_all();

        let (released, release_wake) = &*self.release;
        let mut released = released.lock().expect("release lock");
        while !*released {
            released = release_wake.wait(released).expect("release wait");
        }
        Ok(LLMResponse::with_tool_calls(
            "should be cancelled",
            vec![ToolCall::from_raw_arguments(
                "cancel-finish",
                "task_finish",
                json!({"message": "should be cancelled"}),
            )],
        ))
    }
}

#[derive(Clone)]
struct BlockingCancellationProvider(BlockingCancellationClient);

impl ModelProvider for BlockingCancellationProvider {
    fn resolve(&self, model: &ModelRef) -> Result<ResolvedModelConfig, ModelError> {
        Ok(ResolvedModelConfig::new(
            "blocking",
            model.model(),
            model.model(),
            model.model(),
            Vec::new(),
        ))
    }

    fn client(&self, _resolved: &ResolvedModelConfig) -> Result<Arc<dyn LlmClient>, ModelError> {
        Ok(Arc::new(self.0.clone()))
    }
}

#[tokio::test]
async fn run_handle_cancel_accepted_state_and_terminal_reason_match_fixture() {
    let contract = run_handle_contract();
    let cancellation = &contract["cancellation"];
    let started = Arc::new((Mutex::new(false), Condvar::new()));
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let runner = Runner::builder()
        .model_provider(BlockingCancellationProvider(BlockingCancellationClient {
            started: started.clone(),
            release: release.clone(),
        }))
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("cancel")
        .instructions("Wait.")
        .model(ModelRef::named("cancel-model"))
        .build()
        .expect("agent");
    let handle = runner
        .start(&agent, "go", RunConfig::default())
        .await
        .expect("start");

    {
        let (did_start, started_wake) = &*started;
        let mut did_start = did_start.lock().expect("started lock");
        while !*did_start {
            did_start = started_wake.wait(did_start).expect("started wait");
        }
    }

    let reason = cancellation["reason"].as_str().expect("reason");
    assert!(handle.cancel_with_reason(reason));
    let accepted = handle.state();
    assert_eq!(
        match accepted.status {
            RunHandleStatus::Running => "running",
            _ => "unexpected",
        },
        cancellation["accepted_state"]["status"]
    );
    assert_eq!(accepted.done, cancellation["accepted_state"]["done"]);
    assert_eq!(
        accepted.cancelled,
        cancellation["accepted_state"]["cancelled"]
    );
    assert_eq!(
        handle.cancel_with_reason(reason),
        cancellation["repeated_request_accepted"]
    );

    let (released, release_wake) = &*release;
    *released.lock().expect("release lock") = true;
    release_wake.notify_all();
    let result = handle.result().await.expect("cancelled result");
    let terminal = handle.state();
    assert_eq!(result.status(), AgentStatus::Failed);
    assert_eq!(terminal.status, RunHandleStatus::Cancelled);
    assert_eq!(cancellation["terminal_status"], "cancelled");
    assert!(terminal.done);
    assert!(terminal.cancelled);
    assert!(terminal
        .error
        .as_deref()
        .is_some_and(|error| error.contains(reason)));
    assert_eq!(
        handle.cancel_with_reason(reason),
        cancellation["late_request_accepted"]
    );
}
