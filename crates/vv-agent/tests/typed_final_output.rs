use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde_json::json;
use vv_agent::config::ResolvedModelConfig;
use vv_agent::result::FinalOutputError;
use vv_agent::{
    Agent, AgentResult, EventStoreError, LLMResponse, MemorySession, ModelRef, RunConfig, RunEvent,
    RunEventIter, RunEventPayload, RunEventReplayQuery, RunEventStore, RunResult, Runner,
    ScriptedModelProvider, Session, Span, ToolCall, TraceSink,
};

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct TypedOutput {
    answer: String,
    citations: Vec<String>,
}

#[test]
fn run_result_deserializes_typed_final_output() {
    let result = run_result(AgentResult::completed(
        Vec::new(),
        Vec::new(),
        r#"{"answer":"42","citations":["workspace/README.md"]}"#,
    ));

    let output: TypedOutput = result.deserialize().expect("typed final output");

    assert_eq!(
        output,
        TypedOutput {
            answer: "42".to_string(),
            citations: vec!["workspace/README.md".to_string()],
        }
    );
}

#[test]
fn run_result_exposes_failure_text_as_final_output() {
    let result = run_result(AgentResult::failed("model request failed"));

    assert_eq!(result.final_output(), Some("model request failed"));
    let error = result
        .deserialize::<TypedOutput>()
        .expect_err("invalid JSON must fail");

    assert!(matches!(error, FinalOutputError::Deserialize { .. }));
    assert!(error.to_string().contains("expected value"));
}

#[test]
fn run_result_reports_target_type_and_json_error() {
    let result = run_result(AgentResult::completed(
        Vec::new(),
        Vec::new(),
        r#"{"answer":42}"#,
    ));

    let error = result
        .deserialize::<TypedOutput>()
        .expect_err("invalid output must fail");
    let message = error.to_string();

    assert!(matches!(error, FinalOutputError::Deserialize { .. }));
    assert!(message.contains("typed_final_output::TypedOutput"));
    assert!(message.contains("invalid type: integer `42`, expected a string"));
}

#[test]
fn agent_output_type_installs_a_runtime_validator() {
    let agent = Agent::builder("typed-agent")
        .instructions("Return JSON.")
        .output_type::<TypedOutput>()
        .build()
        .expect("agent");

    assert!(agent
        .output_type_name()
        .is_some_and(|name| name.ends_with("TypedOutput")));
    assert!(agent
        .validate_output(r#"{"answer":"42","citations":[]}"#)
        .is_ok());
    assert!(agent.validate_output(r#"{"answer":42}"#).is_err());
}

#[derive(Clone, Default)]
struct RecordingEventStore {
    events: Arc<Mutex<Vec<RunEvent>>>,
}

impl RunEventStore for RecordingEventStore {
    fn append(&self, event: &RunEvent) -> Result<(), EventStoreError> {
        self.events.lock().expect("events").push(event.clone());
        Ok(())
    }

    fn replay(&self, _query: RunEventReplayQuery) -> Result<RunEventIter, EventStoreError> {
        Ok(Box::new(std::iter::empty()))
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

#[tokio::test]
async fn runner_validates_typed_output_after_persistence_and_terminal_event() {
    let provider = ScriptedModelProvider::new(
        "scripted",
        "typed-model",
        vec![LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "finish",
                "task_finish",
                json!({"message": r#"{"answer":42}"#}),
            )],
        )],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("typed-agent")
        .instructions("Return JSON.")
        .model(ModelRef::named("typed-model"))
        .output_type::<TypedOutput>()
        .build()
        .expect("agent");
    let session = MemorySession::new("typed-output-session");
    let event_store = RecordingEventStore::default();
    let trace_sink = RecordingTraceSink::default();

    let error = match runner
        .run_with_config(
            &agent,
            "go",
            RunConfig::builder()
                .session(session.clone())
                .event_store(Arc::new(event_store.clone()))
                .trace_sink(Arc::new(trace_sink.clone()))
                .build(),
        )
        .await
    {
        Ok(_) => panic!("invalid typed output must fail"),
        Err(error) => error,
    };

    assert!(error.contains("failed to validate final output"));
    assert!(error.contains("typed_final_output::TypedOutput"));
    assert!(error.contains("invalid type: integer `42`, expected a string"));
    assert!(!session
        .get_items(None)
        .await
        .expect("session items")
        .is_empty());
    let events = event_store.events.lock().expect("events");
    assert!(events
        .iter()
        .any(|event| matches!(event.payload(), RunEventPayload::SessionPersisted)));
    assert!(events
        .iter()
        .any(|event| matches!(event.payload(), RunEventPayload::RunCompleted { .. })));
    let ended = trace_sink.ended.lock().expect("ended spans");
    let run_span = ended
        .iter()
        .find(|span| span.name == "run")
        .expect("run span");
    assert_eq!(run_span.metadata["status"], "failed");
    assert!(run_span.metadata["error"]
        .as_str()
        .is_some_and(|error| error.contains("failed to validate final output")));
}

fn run_result(result: AgentResult) -> RunResult {
    RunResult::new(
        "typed-agent",
        result,
        ResolvedModelConfig::new(
            "scripted",
            "demo-model",
            "demo-model",
            "demo-model",
            Vec::new(),
        ),
    )
}
