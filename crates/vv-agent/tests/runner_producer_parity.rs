use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use vv_agent::{
    Agent, FunctionTool, LLMResponse, LlmClient, LlmError, LlmRequest, LlmStreamCallback,
    MemorySession, ModelError, ModelProvider, ModelRef, ResolvedModelConfig, RunConfig, RunEvent,
    RunEventPayload, Runner, ToolCall, ToolOutput,
};

const RUNNER_EVENTS_FIXTURE: &str = include_str!("fixtures/parity/runner_events_v1.jsonl");
const RUNNER_EVENTS_FIXTURE_SHA256: &str =
    "15f23c49cac673766db17c42c96b403d2cc1ece8e876c40d772e8d198bfb8adc";
const PYTHON_RUNNER_TRACE_FIXTURE: &str = include_str!("fixtures/parity/runner_trace_v1.jsonl");
const PYTHON_RUNNER_TRACE_FIXTURE_SHA256: &str =
    "1396aab48578f9f7f0a6f8202efeeef38c36093b0645c11010f7aed7d93cb62b";
const TRACE_FIELDS: &[&str] = &[
    "type",
    "cycle_index",
    "agent_name",
    "model",
    "delta",
    "tool_name",
    "tool_call_id",
    "arguments",
    "status",
    "final_output",
];

#[derive(Clone, Default)]
struct StreamingGoldenClient;

impl LlmClient for StreamingGoldenClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        self.complete_with_stream(request, None)
    }

    fn complete_with_stream(
        &self,
        _request: LlmRequest,
        stream_callback: Option<LlmStreamCallback>,
    ) -> Result<LLMResponse, LlmError> {
        if let Some(callback) = stream_callback {
            callback(&BTreeMap::from([
                ("event".to_string(), json!("assistant_delta")),
                ("content_delta".to_string(), json!("complete ")),
                ("task_id".to_string(), json!("run_spoofed")),
                ("agent_name".to_string(), json!("spoofed-agent")),
                ("session_id".to_string(), json!("session_spoofed")),
            ]));
            callback(&BTreeMap::from([
                ("event".to_string(), json!("assistant_delta")),
                ("content_delta".to_string(), json!("assistant message")),
            ]));
        }
        Ok(LLMResponse::with_tool_calls(
            "complete assistant message",
            vec![ToolCall::new(
                "finish_golden",
                "task_finish",
                BTreeMap::from([("message".to_string(), json!("done"))]),
            )],
        ))
    }
}

#[derive(Clone, Default)]
struct StreamingGoldenProvider;

impl ModelProvider for StreamingGoldenProvider {
    fn resolve(&self, model: &ModelRef) -> Result<ResolvedModelConfig, ModelError> {
        Ok(ResolvedModelConfig::new(
            "golden",
            model.model(),
            model.model(),
            model.model(),
            Vec::new(),
        ))
    }

    fn client(&self, _resolved: &ResolvedModelConfig) -> Result<Arc<dyn LlmClient>, ModelError> {
        Ok(Arc::new(StreamingGoldenClient))
    }
}

#[tokio::test]
async fn real_runner_live_events_match_python_producer_golden() {
    assert_eq!(
        format!("{:x}", Sha256::digest(RUNNER_EVENTS_FIXTURE.as_bytes())),
        RUNNER_EVENTS_FIXTURE_SHA256
    );
    let workspace = tempfile::tempdir().expect("workspace");
    let session = MemorySession::new("session_runner_parity");
    let runner = Runner::builder()
        .model_provider(StreamingGoldenProvider)
        .workspace(workspace.path())
        .build()
        .expect("runner");
    let agent = Agent::builder("runner-agent")
        .instructions("Finish with task_finish.")
        .model(ModelRef::named("golden-model"))
        .build()
        .expect("agent");
    let config = RunConfig::builder()
        .session(session)
        .metadata("trace_id", json!("trace_runner_parity"))
        .build();

    let handle = runner
        .start(&agent, "golden input", config)
        .await
        .expect("start runner");
    let mut stream = handle.events();
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("live event"));
    }
    let result = handle.result().await.expect("runner result");

    assert!(result.run_id().starts_with("run_"));
    assert_ne!(result.run_id(), "run");
    assert_eq!(result.trace_id(), "trace_runner_parity");
    assert_eq!(result.input(), "golden input");
    assert_eq!(result.events(), events);
    assert!(!result.new_items().is_empty());
    assert_eq!(result.token_usage(), &result.result().token_usage);
    assert_eq!(result.metadata()["resolved_model"], "golden-model");
    assert_eq!(result.metadata()["backend"], "golden");
    let mut event_ids = HashSet::new();
    for event in &events {
        assert_eq!(event.run_id(), result.run_id());
        assert_eq!(event.trace_id(), result.trace_id());
        assert_eq!(event.agent_name(), Some("runner-agent"));
        assert_eq!(event.session_id(), Some("session_runner_parity"));
        assert!(event_ids.insert(event.event_id().as_str()));
    }
    assert!(matches!(
        events.first().map(RunEvent::payload),
        Some(RunEventPayload::RunStarted { input }) if input == "golden input"
    ));
    let deltas = events
        .iter()
        .filter_map(|event| match event.payload() {
            RunEventPayload::AssistantDelta { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(deltas, ["complete ", "assistant message"]);

    let actual = events.iter().map(normalize_event).collect::<Vec<_>>();
    let expected = RUNNER_EVENTS_FIXTURE
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("golden fixture event"))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);

    for event in actual {
        if let Some(status) = event.get("status").and_then(Value::as_str) {
            assert_eq!(status, status.to_ascii_lowercase());
        }
    }
}

fn normalize_event(event: &RunEvent) -> Value {
    let mut value = serde_json::to_value(event).expect("serialize live event");
    let object = value.as_object_mut().expect("event object");
    object.insert("event_id".to_string(), json!("evt_dynamic"));
    object.insert("run_id".to_string(), json!("run_dynamic"));
    object.insert("created_at".to_string(), json!(0.0));
    object.remove("metadata");
    value
}

#[tokio::test]
async fn ordinary_run_collects_observability_without_a_live_handle() {
    let workspace = tempfile::tempdir().expect("workspace");
    let runner = Runner::builder()
        .model_provider(StreamingGoldenProvider)
        .workspace(workspace.path())
        .build()
        .expect("runner");
    let agent = Agent::builder("runner-agent")
        .instructions("Finish with task_finish.")
        .model(ModelRef::named("golden-model"))
        .build()
        .expect("agent");

    let result = runner.run(&agent, "golden input").await.expect("run");

    assert_eq!(result.input(), "golden input");
    assert!(!result.new_items().is_empty());
    assert!(!result.events().is_empty());
    assert!(matches!(
        result.events().first().map(RunEvent::payload),
        Some(RunEventPayload::RunStarted { input }) if input == "golden input"
    ));
    assert!(result
        .events()
        .iter()
        .any(|event| matches!(event.payload(), RunEventPayload::RunCompleted { .. })));
    assert_eq!(result.token_usage(), &result.result().token_usage);
    assert_eq!(result.metadata()["resolved_model"], "golden-model");
}

#[derive(Clone, Default)]
struct PythonTraceClient {
    calls: Arc<AtomicUsize>,
}

impl LlmClient for PythonTraceClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        self.complete_with_stream(request, None)
    }

    fn complete_with_stream(
        &self,
        _request: LlmRequest,
        stream_callback: Option<LlmStreamCallback>,
    ) -> Result<LLMResponse, LlmError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let response = match call {
            0 => LLMResponse::with_tool_calls(
                "lookup",
                vec![ToolCall::new(
                    "lookup-call",
                    "lookup",
                    BTreeMap::from([("query".to_string(), json!("parity"))]),
                )],
            ),
            1 => LLMResponse::with_tool_calls(
                "finish",
                vec![ToolCall::new(
                    "finish-call",
                    "task_finish",
                    BTreeMap::from([("message".to_string(), json!("done"))]),
                )],
            ),
            _ => return Err(LlmError::ScriptExhausted),
        };
        if let Some(callback) = stream_callback {
            callback(&BTreeMap::from([
                ("event".to_string(), json!("assistant_delta")),
                ("content_delta".to_string(), json!(response.content.clone())),
            ]));
        }
        Ok(response)
    }
}

#[derive(Clone, Default)]
struct PythonTraceProvider {
    client: PythonTraceClient,
}

impl ModelProvider for PythonTraceProvider {
    fn resolve(&self, model: &ModelRef) -> Result<ResolvedModelConfig, ModelError> {
        Ok(ResolvedModelConfig::new(
            "scripted",
            model.model(),
            model.model(),
            model.model(),
            Vec::new(),
        ))
    }

    fn client(&self, _resolved: &ResolvedModelConfig) -> Result<Arc<dyn LlmClient>, ModelError> {
        Ok(Arc::new(self.client.clone()))
    }
}

#[tokio::test]
async fn real_runner_projection_matches_python_fixture_bytes() {
    assert_eq!(
        format!(
            "{:x}",
            Sha256::digest(PYTHON_RUNNER_TRACE_FIXTURE.as_bytes())
        ),
        PYTHON_RUNNER_TRACE_FIXTURE_SHA256
    );
    let lookup = FunctionTool::builder("lookup")
        .description("Look up a deterministic fixture value.")
        .json_schema(json!({
            "type": "object",
            "properties": {"query": {"type": "string"}},
            "required": ["query"]
        }))
        .handler(|_context, arguments: Value| async move {
            Ok(ToolOutput::text(format!(
                "found:{}",
                arguments["query"].as_str().unwrap_or_default()
            ))
            .with_metadata("producer_marker", json!({"nested": true})))
        })
        .build()
        .expect("lookup tool");
    let workspace = tempfile::tempdir().expect("workspace");
    let runner = Runner::builder()
        .model_provider(PythonTraceProvider::default())
        .workspace(workspace.path())
        .build()
        .expect("runner");
    let agent = Agent::builder("trace-agent")
        .instructions("Use lookup then finish.")
        .model(ModelRef::named("direct"))
        .tool(lookup)
        .build()
        .expect("agent");
    let handle = runner
        .start(
            &agent,
            "trace this",
            RunConfig::builder()
                .session(MemorySession::new("trace-session"))
                .build(),
        )
        .await
        .expect("start");
    let mut stream = handle.events();
    let mut actual = Vec::new();
    let mut saw_lossless_tool_metadata = false;
    while let Some(event) = stream.next().await {
        let event = event.expect("event");
        if matches!(
            event.payload(),
            RunEventPayload::ToolCallCompleted { tool_name, .. } if tool_name == "lookup"
        ) {
            assert_eq!(
                event.metadata()["metadata"]["producer_marker"],
                json!({"nested": true})
            );
            assert_eq!(
                event.metadata()["tool_arguments"],
                json!({"query": "parity"})
            );
            saw_lossless_tool_metadata = true;
        }
        let event_type = serde_json::to_value(&event).expect("event JSON")["type"]
            .as_str()
            .expect("event type")
            .to_string();
        if !matches!(event_type.as_str(), "agent_started" | "session_persisted") {
            actual.push(trace_projection(&event));
        }
    }
    handle.result().await.expect("result");
    assert!(saw_lossless_tool_metadata);
    let expected = PYTHON_RUNNER_TRACE_FIXTURE
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("Python trace fixture"))
        .collect::<Vec<_>>();

    assert_eq!(actual, expected);
}

fn trace_projection(event: &RunEvent) -> Value {
    let event = serde_json::to_value(event).expect("event JSON");
    let projected = TRACE_FIELDS
        .iter()
        .filter_map(|field| {
            event
                .get(*field)
                .cloned()
                .map(|value| ((*field).to_string(), value))
        })
        .collect();
    Value::Object(projected)
}
