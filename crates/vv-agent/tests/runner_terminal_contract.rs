use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use vv_agent::{
    Agent, AgentResult, AgentStatus, EventStoreError, GuardrailOutcome, LLMResponse, MemorySession,
    ModelRef, OutputGuardrail, RunConfig, RunContext, RunEvent, RunEventIter, RunEventPayload,
    RunEventReplayQuery, RunEventStore, Runner, ScriptedModelProvider, ToolCall,
};

const FIXTURE: &str = include_str!("fixtures/parity/runner_terminal_v1.json");
const FIXTURE_SHA256: &str = "4600b26cd3313a790cb84b0a0e1981f9046027b3095450e9b2522db42292939f";

fn contract() -> Value {
    assert_eq!(
        format!("{:x}", Sha256::digest(FIXTURE.as_bytes())),
        FIXTURE_SHA256
    );
    serde_json::from_str(FIXTURE).expect("terminal contract")
}

fn provider() -> ScriptedModelProvider {
    ScriptedModelProvider::new("scripted", "terminal-model", vec![finish_response("done")])
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
        .model_provider(provider())
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
    assert_eq!(result.result().final_answer, None);
    assert_eq!(later_calls.load(Ordering::SeqCst), 0);
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
