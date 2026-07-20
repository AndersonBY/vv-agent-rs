use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde_json::{json, Value};
use vv_agent::{
    Agent, AgentResult, AgentStatus, ApprovalPolicy, CancellationToken, CompletionReason,
    EventStoreError, FunctionTool, GuardrailOutcome, LLMResponse, LlmRequest, ModelRef,
    OutputGuardrail, RunConfig, RunContext, RunEvent, RunEventIter, RunEventPayload,
    RunEventReplayQuery, RunEventStore, Runner, ScriptStep, ScriptedModelProvider, ToolCall,
    ToolOutput, ToolPolicy, ToolUseBehavior,
};

const COMPLETION_CONTRACT: &str = include_str!("fixtures/parity/completion_policy_v1.json");

fn completion_contract() -> Value {
    serde_json::from_str(COMPLETION_CONTRACT).expect("completion contract")
}

fn approval_case(name: &str) -> Value {
    completion_contract()["approval_resume"]["cases"]
        .as_array()
        .expect("approval resume cases")
        .iter()
        .find(|case| case["name"] == name)
        .unwrap_or_else(|| panic!("missing approval case {name}"))
        .clone()
}

struct CountingAllow(Arc<AtomicUsize>);

impl OutputGuardrail for CountingAllow {
    fn check(&self, _context: &RunContext, output: &AgentResult) -> GuardrailOutcome<AgentResult> {
        self.0.fetch_add(1, Ordering::SeqCst);
        GuardrailOutcome::Allow(output.clone())
    }
}

struct BlockingFinishedOutput;

impl OutputGuardrail for BlockingFinishedOutput {
    fn check(&self, _context: &RunContext, output: &AgentResult) -> GuardrailOutcome<AgentResult> {
        if output.completion_reason == Some(CompletionReason::ToolFinish) {
            GuardrailOutcome::Block {
                message: "approval output blocked".to_string(),
            }
        } else {
            GuardrailOutcome::Allow(output.clone())
        }
    }
}

struct RewritingCorruptingAllow {
    rewritten_output: String,
}

impl OutputGuardrail for RewritingCorruptingAllow {
    fn check(&self, _context: &RunContext, output: &AgentResult) -> GuardrailOutcome<AgentResult> {
        let mut corrupted = output.clone();
        corrupted.status = AgentStatus::Completed;
        corrupted.completion_reason = Some(CompletionReason::NoToolFinish);
        corrupted.completion_tool_name = Some("wrong_tool".to_string());
        corrupted.partial_output = Some("wrong partial".to_string());
        corrupted.wait_reason = Some(self.rewritten_output.clone());
        corrupted.final_answer = Some("wrong final".to_string());
        corrupted.error = Some("wrong error".to_string());
        GuardrailOutcome::Allow(corrupted)
    }
}

#[derive(Default)]
struct RecordingEventStore(Mutex<Vec<RunEvent>>);

impl RunEventStore for RecordingEventStore {
    fn append(&self, event: &RunEvent) -> Result<(), EventStoreError> {
        self.0.lock().expect("stored events").push(event.clone());
        Ok(())
    }

    fn replay(&self, _query: RunEventReplayQuery) -> Result<RunEventIter, EventStoreError> {
        Ok(Box::new(
            self.0
                .lock()
                .expect("stored events")
                .clone()
                .into_iter()
                .map(Ok),
        ))
    }
}

#[tokio::test]
async fn direct_approval_wait_runs_output_guardrails() {
    let wait_calls = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(RecordingEventStore::default());
    let wait_runner = runner(vec![LLMResponse::with_tool_calls(
        "assistant wait candidate",
        vec![ToolCall::from_raw_arguments(
            "ask",
            "ask_user",
            json!({"question": "Approved question"}),
        )],
    )]);
    let wait_agent = Agent::builder("approval-agent")
        .instructions("Ask.")
        .model(ModelRef::named("approval-model"))
        .tool_policy(always_approve())
        .output_guardrail(Arc::new(CountingAllow(wait_calls.clone())))
        .build()
        .expect("wait agent");
    let interrupted = wait_runner
        .run_with_config(
            &wait_agent,
            "run",
            RunConfig::builder().event_store(store.clone()).build(),
        )
        .await
        .expect("interrupted");
    let interrupted_run_id = interrupted.run_id().to_string();
    let before_resume = wait_calls.load(Ordering::SeqCst);
    let wait = approve(interrupted, &wait_runner).await;
    assert_eq!(wait.status(), AgentStatus::WaitUser);
    assert_eq!(wait.completion_reason(), Some(CompletionReason::WaitUser));
    assert_eq!(wait.completion_tool_name(), Some("ask_user"));
    assert_eq!(wait_calls.load(Ordering::SeqCst), before_resume + 1);
    assert_resume_terminal_ids(&interrupted_run_id, &wait, &store);
}

#[tokio::test]
async fn output_guardrail_rewrites_wait_output_but_preserves_completion_observation() {
    let contract = completion_contract();
    let case = &contract["output_guardrail_allow"]["case"];
    let candidate = &case["candidate_observation"];
    let expected = &case["expected_observation"];
    let runner = runner(vec![LLMResponse::with_tool_calls(
        candidate["partial_output"]
            .as_str()
            .expect("partial output"),
        vec![ToolCall::from_raw_arguments(
            "ask",
            "ask_user",
            json!({"question": case["candidate_output"]}),
        )],
    )]);
    let agent = Agent::builder("approval-agent")
        .instructions("Ask.")
        .model(ModelRef::named("approval-model"))
        .tool_policy(always_approve())
        .output_guardrail(Arc::new(RewritingCorruptingAllow {
            rewritten_output: case["guardrail_rewrite_output"]
                .as_str()
                .expect("guardrail rewrite")
                .to_string(),
        }))
        .build()
        .expect("agent");
    let interrupted = runner.run(&agent, "run").await.expect("interrupted");

    let resumed = approve(interrupted, &runner).await;

    assert_eq!(
        serde_json::to_value(resumed.status()).expect("result status"),
        expected["status"]
    );
    assert_eq!(
        resumed.completion_reason().map(CompletionReason::as_str),
        expected["completion_reason"].as_str()
    );
    assert_eq!(
        resumed.completion_tool_name(),
        expected["completion_tool_name"].as_str()
    );
    assert_eq!(
        resumed.partial_output(),
        expected["partial_output"].as_str()
    );
    assert_eq!(resumed.final_output(), case["expected_output"].as_str());
    assert_eq!(resumed.result().error, None);
}

#[tokio::test]
async fn direct_approval_finish_runs_output_guardrails() {
    let finish_calls = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(RecordingEventStore::default());
    let finish_runner = runner(vec![finish_response(
        "finish",
        "assistant finish candidate",
        "approved finish",
    )]);
    let finish_agent = Agent::builder("approval-agent")
        .instructions("Finish.")
        .model(ModelRef::named("approval-model"))
        .tool_policy(always_approve())
        .output_guardrail(Arc::new(CountingAllow(finish_calls.clone())))
        .build()
        .expect("finish agent");
    let interrupted = finish_runner
        .run_with_config(
            &finish_agent,
            "run",
            RunConfig::builder().event_store(store.clone()).build(),
        )
        .await
        .expect("interrupted");
    let interrupted_run_id = interrupted.run_id().to_string();
    let before_resume = finish_calls.load(Ordering::SeqCst);
    let finish = approve(interrupted, &finish_runner).await;
    assert_eq!(finish.status(), AgentStatus::Completed);
    assert_eq!(
        finish.completion_reason(),
        Some(CompletionReason::ToolFinish)
    );
    assert_eq!(finish.completion_tool_name(), Some("task_finish"));
    assert_eq!(finish_calls.load(Ordering::SeqCst), before_resume + 1);
    assert_resume_terminal_ids(&interrupted_run_id, &finish, &store);
}

#[tokio::test]
async fn direct_approval_stop_policy_runs_output_guardrails() {
    let stop_calls = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(RecordingEventStore::default());
    let guarded = FunctionTool::builder("guarded_lookup")
        .needs_approval(true)
        .handler(|_context, _arguments: Value| async { Ok(ToolOutput::text("lookup result")) })
        .build()
        .expect("guarded lookup");
    let stop_runner = runner(vec![LLMResponse::with_tool_calls(
        "assistant stop candidate",
        vec![ToolCall::from_raw_arguments(
            "lookup",
            "guarded_lookup",
            json!({}),
        )],
    )]);
    let stop_agent = Agent::builder("approval-agent")
        .instructions("Lookup.")
        .model(ModelRef::named("approval-model"))
        .tool(guarded)
        .tool_policy(ToolPolicy {
            approval: ApprovalPolicy::OnRequest,
            ..ToolPolicy::default()
        })
        .tool_use_behavior(ToolUseBehavior::StopOnFirstTool)
        .output_guardrail(Arc::new(CountingAllow(stop_calls.clone())))
        .build()
        .expect("stop agent");
    let interrupted = stop_runner
        .run_with_config(
            &stop_agent,
            "run",
            RunConfig::builder().event_store(store.clone()).build(),
        )
        .await
        .expect("interrupted");
    let interrupted_run_id = interrupted.run_id().to_string();
    let before_resume = stop_calls.load(Ordering::SeqCst);
    let stop = approve(interrupted, &stop_runner).await;
    assert_eq!(stop.status(), AgentStatus::Completed);
    assert_eq!(
        stop.completion_reason(),
        Some(CompletionReason::StopOnFirstTool)
    );
    assert_eq!(stop.completion_tool_name(), Some("guarded_lookup"));
    assert_eq!(stop_calls.load(Ordering::SeqCst), before_resume + 1);
    assert_resume_terminal_ids(&interrupted_run_id, &stop, &store);
}

#[tokio::test]
async fn approval_terminal_result_and_event_store_end_with_guardrail_status() {
    let store = Arc::new(RecordingEventStore::default());
    let runner = runner(vec![finish_response(
        "finish",
        "assistant approval candidate",
        "tool output must not become partial",
    )]);
    let agent = Agent::builder("approval-agent")
        .instructions("Finish.")
        .model(ModelRef::named("approval-model"))
        .tool_policy(always_approve())
        .output_guardrail(Arc::new(BlockingFinishedOutput))
        .build()
        .expect("agent");
    let interrupted = runner
        .run_with_config(
            &agent,
            "run",
            RunConfig::builder().event_store(store.clone()).build(),
        )
        .await
        .expect("interrupted");
    let interrupted_run_id = interrupted.run_id().to_string();
    let resumed = approve(interrupted, &runner).await;

    assert_eq!(resumed.status(), AgentStatus::Failed);
    assert_eq!(resumed.completion_reason(), Some(CompletionReason::Failed));
    assert_eq!(
        resumed.partial_output(),
        Some("assistant approval candidate")
    );
    assert_eq!(resumed.final_output(), Some("approval output blocked"));
    assert_resume_terminal_ids(&interrupted_run_id, &resumed, &store);
    let last = resumed.events().last().expect("last result event");
    assert!(matches!(last.payload(), RunEventPayload::RunFailed { .. }));
    assert_eq!(last.completion_reason(), Some(CompletionReason::Failed));
    assert_eq!(last.partial_output(), resumed.partial_output());

    let stored = store.0.lock().expect("stored events");
    let last = stored.last().expect("last stored event");
    assert!(matches!(last.payload(), RunEventPayload::RunFailed { .. }));
    assert_eq!(last.completion_reason(), Some(CompletionReason::Failed));
    assert_ne!(last.completion_reason(), Some(CompletionReason::WaitUser));
}

#[tokio::test]
async fn approved_error_continue_is_returned_to_the_llm() {
    let contract = approval_case("approved_continue_uses_full_fresh_cycle_budget");
    let expected = &contract["expected"];
    let requests = Arc::new(Mutex::new(Vec::<LlmRequest>::new()));
    let store = Arc::new(RecordingEventStore::default());
    let first_requests = requests.clone();
    let second_requests = requests.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "approval-model",
        vec![
            ScriptStep::callback(move |request| {
                first_requests
                    .lock()
                    .expect("requests")
                    .push(request.clone());
                Ok(LLMResponse::with_tool_calls(
                    "try guarded tool",
                    vec![ToolCall::from_raw_arguments(
                        "guarded_error",
                        "guarded_error",
                        json!({}),
                    )],
                ))
            }),
            ScriptStep::callback(move |request| {
                second_requests
                    .lock()
                    .expect("requests")
                    .push(request.clone());
                Ok(finish_response(
                    "recovered",
                    "finish after tool error",
                    "recovered after approved error",
                ))
            }),
        ],
    );
    let tool = FunctionTool::builder("guarded_error")
        .needs_approval(true)
        .handler(|_context, _arguments: Value| async {
            Ok(ToolOutput::error("approved tool failed").with_code("approved_failure"))
        })
        .build()
        .expect("tool");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("approval-agent")
        .instructions("Recover from tool errors.")
        .model(ModelRef::named("approval-model"))
        .tool(tool)
        .tool_policy(ToolPolicy {
            approval: ApprovalPolicy::OnRequest,
            ..ToolPolicy::default()
        })
        .build()
        .expect("agent");

    let interrupted = runner
        .run_with_config(
            &agent,
            "run",
            RunConfig::builder()
                .max_cycles(
                    contract["configured_max_cycles"]
                        .as_u64()
                        .expect("configured max cycles") as u32,
                )
                .event_store(store.clone())
                .build(),
        )
        .await
        .expect("interrupted");
    let interrupted_run_id = interrupted.run_id().to_string();
    let interrupted_trace_id = interrupted.trace_id().to_string();
    let resumed = approve(interrupted, &runner).await;

    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_ne!(resumed.run_id(), interrupted_run_id);
    assert_eq!(resumed.trace_id(), interrupted_trace_id);
    assert_eq!(
        resumed.completion_reason().map(|reason| reason.as_str()),
        expected["completion_reason"].as_str()
    );
    assert_eq!(
        resumed.result().cycles.len(),
        expected["cycles"].as_u64().expect("expected cycles") as usize
    );
    assert_eq!(
        resumed.final_output(),
        Some("recovered after approved error")
    );
    assert_eq!(terminal_count(resumed.events(), &interrupted_run_id), 0);
    assert_eq!(terminal_count(resumed.events(), resumed.run_id()), 1);
    let resumed_tool_lifecycle = resumed
        .events()
        .iter()
        .filter(|event| event.run_id() == resumed.run_id())
        .filter_map(|event| match event.payload() {
            RunEventPayload::ToolCallPlanned { tool_call_id, .. }
                if tool_call_id == "guarded_error" =>
            {
                Some("planned")
            }
            RunEventPayload::ToolCallStarted { tool_call_id, .. }
                if tool_call_id == "guarded_error" =>
            {
                Some("started")
            }
            RunEventPayload::ToolCallCompleted { tool_call_id, .. }
                if tool_call_id == "guarded_error" =>
            {
                Some("completed")
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(resumed_tool_lifecycle, ["planned", "started", "completed"]);
    let stored = store.0.lock().expect("stored events");
    assert_eq!(terminal_count(&stored, &interrupted_run_id), 1);
    assert_eq!(terminal_count(&stored, resumed.run_id()), 1);
    drop(stored);
    let requests = requests.lock().expect("requests");
    assert_eq!(requests.len(), 2);
    assert!(requests[1].messages.iter().any(|message| {
        message.tool_call_id.as_deref() == Some("guarded_error")
            && message.content.contains("approved tool failed")
    }));
}

#[tokio::test]
async fn approved_resume_rejects_input_before_consuming_the_shared_claim() {
    let contract = approval_case("approved_resume_rejects_input_before_claim");
    let expected = &contract["expected"];
    let executions = Arc::new(AtomicUsize::new(0));
    let tool_executions = executions.clone();
    let guarded = FunctionTool::builder("guarded")
        .needs_approval(true)
        .handler(move |_context, _arguments: Value| {
            let executions = tool_executions.clone();
            async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text("approved"))
            }
        })
        .build()
        .expect("guarded tool");
    let runner = runner(vec![LLMResponse::with_tool_calls(
        "guard",
        vec![ToolCall::from_raw_arguments("guard", "guarded", json!({}))],
    )]);
    let agent = Agent::builder("approval-agent")
        .instructions("Run once.")
        .model(ModelRef::named("approval-model"))
        .tool(guarded)
        .tool_use_behavior(ToolUseBehavior::StopOnFirstTool)
        .build()
        .expect("agent");
    let interrupted = runner.run(&agent, "run").await.expect("interrupted");
    let interruption_id = interrupted.approvals()[0].interruption_id.clone();
    let mut state = interrupted.into_state().expect("state");
    state.approve(&interruption_id).expect("approve");
    let retry = state.clone();

    let error = match runner
        .resume_with_input(
            state,
            contract["resume_input"].as_str().expect("resume input"),
        )
        .await
    {
        Ok(_) => panic!("approved input must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error, expected["error"].as_str().expect("expected error"));
    assert_eq!(
        executions.load(Ordering::SeqCst),
        expected["tool_execution_count"]
            .as_u64()
            .expect("tool execution count") as usize
    );

    let resumed = runner.resume(retry).await.expect("claim remains available");
    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_eq!(resumed.final_output(), Some("approved"));
    assert_eq!(executions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn pre_cancelled_approved_resume_with_input_rejects_before_cancellation() {
    let contract =
        approval_case("pre_cancelled_approved_resume_with_input_rejects_before_cancellation");
    let expected = &contract["expected"];
    let executions = Arc::new(AtomicUsize::new(0));
    let tool_executions = executions.clone();
    let guardrail_calls = Arc::new(AtomicUsize::new(0));
    let token = CancellationToken::default();
    let store = Arc::new(RecordingEventStore::default());
    let guarded = FunctionTool::builder("guarded")
        .needs_approval(true)
        .handler(move |_context, _arguments: Value| {
            let executions = tool_executions.clone();
            async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text("executed"))
            }
        })
        .build()
        .expect("guarded tool");
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "approval-model",
            vec![LLMResponse::with_tool_calls(
                "guard",
                vec![ToolCall::from_raw_arguments("guard", "guarded", json!({}))],
            )],
        ))
        .workspace("./workspace")
        .default_run_config(
            RunConfig::builder()
                .cancellation_token(token.clone())
                .build(),
        )
        .build()
        .expect("runner");
    let agent = Agent::builder("approval-agent")
        .instructions("Run after approval.")
        .model(ModelRef::named("approval-model"))
        .tool(guarded)
        .output_guardrail(Arc::new(CountingAllow(guardrail_calls.clone())))
        .build()
        .expect("agent");
    let interrupted = runner
        .run_with_config(
            &agent,
            "run",
            RunConfig::builder().event_store(store.clone()).build(),
        )
        .await
        .expect("interrupted");
    let interrupted_run_id = interrupted.run_id().to_string();
    let interruption_id = interrupted.approvals()[0].interruption_id.clone();
    let mut state = interrupted.into_state().expect("state");
    state.approve(&interruption_id).expect("approve");
    let guardrail_calls_before_resume = guardrail_calls.load(Ordering::SeqCst);
    token.cancel_with_reason(
        contract["cancellation_reason"]
            .as_str()
            .expect("cancellation reason"),
    );

    let error = match runner
        .resume_with_input(
            state,
            contract["resume_input"].as_str().expect("resume input"),
        )
        .await
    {
        Ok(_) => panic!("invalid approval input must win over cancellation"),
        Err(error) => error,
    };

    assert_eq!(error, expected["error"].as_str().expect("expected error"));
    assert_eq!(
        executions.load(Ordering::SeqCst),
        expected["tool_execution_count"]
            .as_u64()
            .expect("tool execution count") as usize
    );
    assert_eq!(
        guardrail_calls.load(Ordering::SeqCst) - guardrail_calls_before_resume,
        expected["output_guardrail_count"]
            .as_u64()
            .expect("guardrail count") as usize
    );
    let stored = store.0.lock().expect("stored events");
    let fresh_terminals = stored
        .iter()
        .filter(|event| event.run_id() != interrupted_run_id && is_terminal(event))
        .count();
    assert_eq!(
        fresh_terminals,
        expected["terminal_count"].as_u64().expect("terminal count") as usize
    );
}

#[tokio::test]
async fn pre_cancelled_approved_resume_skips_tool_and_guardrail_and_emits_one_terminal() {
    let contract = approval_case("pre_cancelled_approved_resume_has_no_side_effects");
    let expected = &contract["expected"];
    let executions = Arc::new(AtomicUsize::new(0));
    let tool_executions = executions.clone();
    let guardrail_calls = Arc::new(AtomicUsize::new(0));
    let token = CancellationToken::default();
    let store = Arc::new(RecordingEventStore::default());
    let guarded = FunctionTool::builder("guarded")
        .needs_approval(true)
        .handler(move |_context, _arguments: Value| {
            let executions = tool_executions.clone();
            async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text("unsafe"))
            }
        })
        .build()
        .expect("guarded tool");
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "approval-model",
            vec![LLMResponse::with_tool_calls(
                "assistant draft before approval",
                vec![ToolCall::from_raw_arguments("guard", "guarded", json!({}))],
            )],
        ))
        .workspace("./workspace")
        .default_run_config(
            RunConfig::builder()
                .cancellation_token(token.clone())
                .build(),
        )
        .build()
        .expect("runner");
    let agent = Agent::builder("approval-agent")
        .instructions("Run only after approval.")
        .model(ModelRef::named("approval-model"))
        .tool(guarded)
        .tool_use_behavior(ToolUseBehavior::StopOnFirstTool)
        .output_guardrail(Arc::new(CountingAllow(guardrail_calls.clone())))
        .build()
        .expect("agent");
    let interrupted = runner
        .run_with_config(
            &agent,
            "run",
            RunConfig::builder().event_store(store.clone()).build(),
        )
        .await
        .expect("interrupted");
    let interrupted_run_id = interrupted.run_id().to_string();
    let interrupted_trace_id = interrupted.trace_id().to_string();
    let before_resume = guardrail_calls.load(Ordering::SeqCst);
    let interruption_id = interrupted.approvals()[0].interruption_id.clone();
    let mut state = interrupted.into_state().expect("state");
    state.approve(&interruption_id).expect("approve");
    token.cancel_with_reason(
        contract["cancellation_reason"]
            .as_str()
            .expect("cancellation reason"),
    );

    let resumed = runner.resume(state).await.expect("cancelled result");

    assert_eq!(resumed.trace_id(), interrupted_trace_id);
    assert_eq!(
        serde_json::to_value(resumed.status()).expect("result status"),
        expected["status"]
    );
    assert_eq!(
        resumed.completion_reason().map(|reason| reason.as_str()),
        expected["completion_reason"].as_str()
    );
    assert_eq!(
        resumed.completion_tool_name(),
        expected["completion_tool_name"].as_str()
    );
    assert_eq!(
        resumed.partial_output(),
        expected["partial_output"].as_str()
    );
    assert_eq!(resumed.final_output(), expected["final_output"].as_str());
    assert_eq!(
        executions.load(Ordering::SeqCst),
        expected["tool_execution_count"]
            .as_u64()
            .expect("tool execution count") as usize
    );
    assert_eq!(
        guardrail_calls.load(Ordering::SeqCst) - before_resume,
        expected["output_guardrail_count"]
            .as_u64()
            .expect("guardrail count") as usize
    );
    assert_resume_terminal_ids(&interrupted_run_id, &resumed, &store);
    assert!(matches!(
        resumed.events().last().expect("cancel terminal").payload(),
        RunEventPayload::RunCancelled { .. }
    ));
    assert_eq!(
        terminal_count(resumed.events(), resumed.run_id()),
        expected["terminal_count"].as_u64().expect("terminal count") as usize
    );
}

#[derive(Debug, Deserialize)]
struct TypedApprovalOutput {
    #[serde(rename = "answer")]
    _answer: String,
}

#[tokio::test]
async fn approval_typed_output_error_is_raised_after_fresh_terminal_is_persisted() {
    let contract = approval_case("approval_typed_output_error_follows_fresh_terminal");
    let expected = &contract["expected"];
    let store = Arc::new(RecordingEventStore::default());
    let runner = runner(vec![finish_response(
        "typed-finish",
        "typed candidate",
        r#"{"answer":42}"#,
    )]);
    let agent = Agent::builder("approval-agent")
        .instructions("Return typed output.")
        .model(ModelRef::named("approval-model"))
        .tool_policy(always_approve())
        .output_type::<TypedApprovalOutput>()
        .build()
        .expect("agent");
    let interrupted = runner
        .run_with_config(
            &agent,
            "run",
            RunConfig::builder().event_store(store.clone()).build(),
        )
        .await
        .expect("interrupted");
    let interrupted_run_id = interrupted.run_id().to_string();
    let interruption_id = interrupted.approvals()[0].interruption_id.clone();
    let mut state = interrupted.into_state().expect("state");
    state.approve(&interruption_id).expect("approve");
    let retry = state.clone();

    let error = match runner.resume(state).await {
        Ok(_) => panic!("invalid typed output must fail"),
        Err(error) => error,
    };
    assert!(error.contains(
        expected["error_contains"]
            .as_str()
            .expect("typed output error")
    ));
    {
        let stored = store.0.lock().expect("stored events");
        let fresh_terminal = stored.last().expect("fresh terminal");
        assert_eq!(
            fresh_terminal.run_id() != interrupted_run_id,
            expected["fresh_run_id"].as_bool().expect("fresh run id")
        );
        assert_eq!(terminal_count(&stored, fresh_terminal.run_id()), 1);
        assert_eq!(
            fresh_terminal.completion_reason(),
            Some(CompletionReason::ToolFinish)
        );
    }

    let retry_error = match runner.resume(retry).await {
        Ok(_) => panic!("executed approval cannot be replayed"),
        Err(error) => error,
    };
    assert_eq!(retry_error, "approval_already_consumed");
    assert_eq!(expected["approval_claim_consumed"], Value::Bool(true));
}

fn runner(responses: Vec<LLMResponse>) -> Runner {
    Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "approval-model",
            responses,
        ))
        .workspace("./workspace")
        .build()
        .expect("runner")
}

fn always_approve() -> ToolPolicy {
    ToolPolicy {
        approval: ApprovalPolicy::Always,
        ..ToolPolicy::default()
    }
}

async fn approve(interrupted: vv_agent::RunResult, runner: &Runner) -> vv_agent::RunResult {
    assert_eq!(interrupted.status(), AgentStatus::WaitUser);
    let interruption_id = interrupted.approvals()[0].interruption_id.clone();
    let mut state = interrupted.into_state().expect("state");
    state.approve(&interruption_id).expect("approve");
    runner.resume(state).await.expect("resume")
}

fn finish_response(id: &str, assistant_output: &str, final_output: &str) -> LLMResponse {
    LLMResponse::with_tool_calls(
        assistant_output,
        vec![ToolCall::from_raw_arguments(
            id,
            "task_finish",
            json!({"message": final_output}),
        )],
    )
}

fn assert_resume_terminal_ids(
    interrupted_run_id: &str,
    resumed: &vv_agent::RunResult,
    store: &Arc<RecordingEventStore>,
) {
    assert_ne!(resumed.run_id(), interrupted_run_id);
    assert_eq!(terminal_count(resumed.events(), interrupted_run_id), 1);
    assert_eq!(terminal_count(resumed.events(), resumed.run_id()), 1);

    let stored = store.0.lock().expect("stored events");
    assert_eq!(terminal_count(&stored, interrupted_run_id), 1);
    assert_eq!(terminal_count(&stored, resumed.run_id()), 1);
    assert_eq!(
        stored.last().expect("last stored event").run_id(),
        resumed.run_id()
    );
}

fn terminal_count(events: &[RunEvent], run_id: &str) -> usize {
    events
        .iter()
        .filter(|event| event.run_id() == run_id && is_terminal(event))
        .count()
}

fn is_terminal(event: &RunEvent) -> bool {
    matches!(
        event.payload(),
        RunEventPayload::RunCompleted { .. }
            | RunEventPayload::RunFailed { .. }
            | RunEventPayload::RunCancelled { .. }
    )
}
