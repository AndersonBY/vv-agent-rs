use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use vv_agent::{
    Agent, AgentResult, AgentStatus, CompletionReason, EventStoreError, GuardrailOutcome,
    LLMResponse, MemorySession, ModelRef, OutputGuardrail, RunConfig, RunContext, RunEvent,
    RunEventIter, RunEventPayload, RunEventReplayQuery, RunEventStore, Runner,
    ScriptedModelProvider, ToolCall,
};

const FIXTURE: &str = include_str!("fixtures/parity/runner_terminal_v1.json");
const FIXTURE_SHA256: &str = "927c76bcb770364314fd42966a942b552ec6f3ccc1afcdcc419c571358ffc3de";
const COMPLETION_FIXTURE: &str = include_str!("fixtures/parity/completion_policy_v1.json");

fn contract() -> Value {
    assert_eq!(
        format!("{:x}", Sha256::digest(FIXTURE.as_bytes())),
        FIXTURE_SHA256
    );
    serde_json::from_str(FIXTURE).expect("terminal contract")
}

fn completion_contract() -> Value {
    serde_json::from_str(COMPLETION_FIXTURE).expect("completion contract")
}

fn provider() -> ScriptedModelProvider {
    ScriptedModelProvider::new("scripted", "terminal-model", vec![finish_response("done")])
}

fn guarded_provider() -> ScriptedModelProvider {
    ScriptedModelProvider::new(
        "scripted",
        "terminal-model",
        vec![LLMResponse::with_tool_calls(
            "blocked final output candidate",
            vec![ToolCall::new(
                "finish",
                "task_finish",
                BTreeMap::from([("message".to_string(), json!("tool final output"))]),
            )],
        )],
    )
}

fn agent() -> Agent {
    Agent::builder("terminal-agent")
        .instructions("Finish.")
        .model(ModelRef::named("terminal-model"))
        .build()
        .expect("agent")
}

#[tokio::test]
async fn session_persists_before_the_only_success_terminal() {
    let expected = &contract()["success_with_session"];
    let runner = Runner::builder()
        .model_provider(provider())
        .workspace("./workspace")
        .build()
        .expect("runner");
    let result = runner
        .run_with_config(
            &agent(),
            "go",
            RunConfig::builder()
                .session(MemorySession::new("terminal-session"))
                .build(),
        )
        .await
        .expect("run");
    let types = result.events().iter().map(event_type).collect::<Vec<_>>();
    let terminals = result
        .events()
        .iter()
        .filter(terminal)
        .map(event_type)
        .collect::<Vec<_>>();

    assert_eq!(
        types[types.len() - 2..],
        ["session_persisted", "run_completed"]
    );
    assert_eq!(
        terminals,
        [expected["terminal"].as_str().expect("terminal")]
    );
    assert_eq!(result.status(), AgentStatus::Completed);
}

struct BlockOutput;

impl OutputGuardrail for BlockOutput {
    fn check(&self, _context: &RunContext, _output: &AgentResult) -> GuardrailOutcome<AgentResult> {
        GuardrailOutcome::Block {
            message: "blocked final output".to_string(),
        }
    }
}

struct LaterOutput(Arc<AtomicUsize>);

impl OutputGuardrail for LaterOutput {
    fn check(&self, _context: &RunContext, output: &AgentResult) -> GuardrailOutcome<AgentResult> {
        self.0.fetch_add(1, Ordering::SeqCst);
        GuardrailOutcome::Allow(output.clone())
    }
}

#[tokio::test]
async fn output_guardrail_block_short_circuits_and_owns_final_terminal() {
    let expected = &contract()["output_guardrail_block"];
    let later_calls = Arc::new(AtomicUsize::new(0));
    let guarded = Agent::builder("terminal-agent")
        .instructions("Finish.")
        .model(ModelRef::named("terminal-model"))
        .output_guardrail(Arc::new(BlockOutput))
        .output_guardrail(Arc::new(LaterOutput(later_calls.clone())))
        .build()
        .expect("agent");
    let runner = Runner::builder()
        .model_provider(guarded_provider())
        .workspace("./workspace")
        .build()
        .expect("runner");
    let result = runner
        .run_with_config(
            &guarded,
            "go",
            RunConfig::builder()
                .session(MemorySession::new("blocked-session"))
                .build(),
        )
        .await
        .expect("run");
    let types = result.events().iter().map(event_type).collect::<Vec<_>>();
    let terminals = result
        .events()
        .iter()
        .filter(terminal)
        .map(event_type)
        .collect::<Vec<_>>();

    assert_eq!(
        types[types.len() - 2..],
        ["session_persisted", "run_failed"]
    );
    assert_eq!(
        terminals,
        [expected["terminal"].as_str().expect("terminal")]
    );
    assert_eq!(result.status(), AgentStatus::Failed);
    assert_eq!(result.final_output(), expected["error"].as_str());
    assert_eq!(result.completion_reason(), Some(CompletionReason::Failed));
    assert_eq!(result.partial_output(), expected["partial_output"].as_str());
    assert_ne!(result.partial_output(), Some("tool final output"));
    assert_eq!(result.result().final_answer, None);
    let terminal = result.events().iter().find(terminal).unwrap();
    assert_eq!(terminal.completion_reason(), Some(CompletionReason::Failed));
    assert_eq!(terminal.partial_output(), result.partial_output());
    assert_eq!(later_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn llm_call_failure_returns_typed_failed_result_and_terminal_event() {
    let completion = completion_contract();
    let expected = &completion["ordinary_llm_failure"];
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "terminal-model",
            Vec::new(),
        ))
        .workspace("./workspace")
        .build()
        .expect("runner");

    let result = runner.run(&agent(), "go").await.expect("failed result");

    assert_eq!(expected["runner_outcome"], "typed_result");
    assert_eq!(
        serde_json::to_value(result.status()).expect("result status"),
        expected["status"]
    );
    assert_eq!(
        result.completion_reason().map(|reason| reason.as_str()),
        expected["completion_reason"].as_str()
    );
    assert_eq!(
        result.completion_tool_name(),
        expected["completion_tool_name"].as_str()
    );
    assert_eq!(result.partial_output(), expected["partial_output"].as_str());
    assert_eq!(
        result.final_output(),
        Some("scripted response queue is empty")
    );
    let terminals = result.events().iter().filter(terminal).collect::<Vec<_>>();
    assert_eq!(
        terminals.len(),
        expected["terminal_count"].as_u64().expect("terminal count") as usize
    );
    assert!(matches!(
        terminals[0].payload(),
        RunEventPayload::RunFailed { .. }
    ));
    assert_eq!(
        terminals[0].completion_reason(),
        Some(CompletionReason::Failed)
    );
    assert_eq!(
        event_type(terminals[0]),
        expected["terminal_event"].as_str().expect("terminal event")
    );
}

struct FailingStore;

impl RunEventStore for FailingStore {
    fn append(&self, _event: &RunEvent) -> Result<(), EventStoreError> {
        Err(EventStoreError::new("event_store_test_error", "store down"))
    }

    fn replay(&self, _query: RunEventReplayQuery) -> Result<RunEventIter, EventStoreError> {
        Ok(Box::new(std::iter::empty()))
    }
}

#[tokio::test]
async fn event_store_fail_closed_is_a_normal_runner_error() {
    let expected = &contract()["event_store_fail_closed"];
    let runner = Runner::builder()
        .model_provider(provider())
        .workspace("./workspace")
        .build()
        .expect("runner");
    let error = match runner
        .run_with_config(
            &agent(),
            "go",
            RunConfig::builder()
                .event_store(Arc::new(FailingStore))
                .event_store_fail_closed(true)
                .build(),
        )
        .await
    {
        Ok(_) => panic!("fail-closed event store must fail the run"),
        Err(error) => error,
    };

    assert_eq!(error, expected["error"].as_str().expect("error"));
}

fn finish_response(message: &str) -> LLMResponse {
    LLMResponse::with_tool_calls(
        "",
        vec![ToolCall::new(
            "finish",
            "task_finish",
            BTreeMap::from([("message".to_string(), json!(message))]),
        )],
    )
}

fn event_type(event: &RunEvent) -> &'static str {
    match event.payload() {
        RunEventPayload::SessionPersisted => "session_persisted",
        RunEventPayload::RunCompleted { .. } => "run_completed",
        RunEventPayload::RunFailed { .. } => "run_failed",
        RunEventPayload::RunCancelled { .. } => "run_cancelled",
        _ => "other",
    }
}

fn terminal(event: &&RunEvent) -> bool {
    matches!(
        event.payload(),
        RunEventPayload::RunCompleted { .. }
            | RunEventPayload::RunFailed { .. }
            | RunEventPayload::RunCancelled { .. }
    )
}
