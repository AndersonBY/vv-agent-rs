use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use vv_agent::{
    Agent, FunctionTool, LLMResponse, LlmClient, LlmError, LlmRequest, LlmStreamCallback,
    MemorySession, ModelError, ModelProvider, ModelRef, NoToolPolicy, ResolvedModelConfig,
    RunConfig, RunEvent, RunEventPayload, Runner, ToolCall, ToolOutput,
};

const RUNNER_EVENTS_FIXTURE: &str = include_str!("fixtures/parity/runner_events_v1.jsonl");
const RUNNER_EVENTS_FIXTURE_SHA256: &str =
    "76903d30f5f55697a7364f2b0c3caa66bb0205b14012a7e10c3c2dcf07d81b32";
const STREAM_PROJECTION_FIXTURE: &str = include_str!("fixtures/parity/stream_projection_v1.json");
const STREAM_PROJECTION_FIXTURE_SHA256: &str =
    "95a3b7d527efad68492d810038ff7be0eecefcd278f8ddad690359c497d79b0a";
const PYTHON_RUNNER_TRACE_FIXTURE: &str = include_str!("fixtures/parity/runner_trace_v1.jsonl");
const PYTHON_RUNNER_TRACE_FIXTURE_SHA256: &str =
    "998fb1341dbecc29d1a3ddef95bc9b38a180510485f52c57ad4afcef2d576834";
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
    "directive",
    "error_code",
    "execution_started",
    "duration_ms",
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

#[derive(Clone)]
struct ContractStreamClient {
    calls: Arc<AtomicUsize>,
    raw_events: Arc<Vec<BTreeMap<String, Value>>>,
}

impl Default for ContractStreamClient {
    fn default() -> Self {
        let fixture: Value =
            serde_json::from_str(STREAM_PROJECTION_FIXTURE).expect("stream projection fixture");
        let raw_events = fixture["synthetic_top_level"]["raw_events"]
            .as_array()
            .expect("synthetic raw events")
            .iter()
            .map(raw_event_map)
            .collect();
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            raw_events: Arc::new(raw_events),
        }
    }
}

impl ContractStreamClient {
    fn with_raw_events(raw_events: Vec<BTreeMap<String, Value>>) -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            raw_events: Arc::new(raw_events),
        }
    }
}

impl LlmClient for ContractStreamClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        self.complete_with_stream(request, None)
    }

    fn complete_with_stream(
        &self,
        _request: LlmRequest,
        stream_callback: Option<LlmStreamCallback>,
    ) -> Result<LLMResponse, LlmError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call < 3 {
            return Ok(LLMResponse::new(format!("draft {call}")));
        }

        let callback = stream_callback.expect("contract stream callback");
        for raw_event in self.raw_events.iter() {
            callback(raw_event);
        }
        Ok(LLMResponse::with_tool_calls(
            "done",
            vec![ToolCall::new(
                "call_stream",
                "task_finish",
                BTreeMap::from([("message".to_string(), json!("done"))]),
            )],
        ))
    }
}

#[derive(Clone, Default)]
struct ContractStreamProvider {
    client: ContractStreamClient,
}

impl ModelProvider for ContractStreamProvider {
    fn resolve(&self, model: &ModelRef) -> Result<ResolvedModelConfig, ModelError> {
        Ok(ResolvedModelConfig::new(
            "stream",
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
async fn real_runner_projects_contract_stream_fixture_with_framework_identity() {
    assert_eq!(
        format!("{:x}", Sha256::digest(STREAM_PROJECTION_FIXTURE.as_bytes())),
        STREAM_PROJECTION_FIXTURE_SHA256
    );
    let fixture: Value =
        serde_json::from_str(STREAM_PROJECTION_FIXTURE).expect("stream projection fixture");
    let synthetic = &fixture["synthetic_top_level"];
    let raw_observed = Arc::new(std::sync::Mutex::new(Vec::<BTreeMap<String, Value>>::new()));
    let raw_observed_for_callback = raw_observed.clone();
    let provider = ContractStreamProvider::default();
    let calls = provider.client.calls.clone();
    let workspace = tempfile::tempdir().expect("workspace");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(workspace.path())
        .build()
        .expect("runner");
    let agent = Agent::builder("stream-agent")
        .instructions("Finish on the third cycle.")
        .model(ModelRef::named("stream-model"))
        .build()
        .expect("agent");
    let config = RunConfig::builder()
        .session(MemorySession::new("session_stream_parity"))
        .max_cycles(3)
        .no_tool_policy(NoToolPolicy::Continue)
        .metadata("trace_id", json!("trace_stream_parity"))
        .runtime_stream_callback(move |payload| {
            raw_observed_for_callback
                .lock()
                .expect("raw stream observations")
                .push(payload.clone());
        })
        .build();

    let result = runner
        .run_with_config(&agent, "stream input", config)
        .await
        .expect("contract stream run");
    let typed_events = result
        .events()
        .iter()
        .filter(|event| is_typed_stream_event(event.payload()))
        .collect::<Vec<_>>();
    let actual = typed_events
        .iter()
        .map(|event| normalize_event(event))
        .collect::<Vec<_>>();
    let expected = synthetic["expected_wire_events"]
        .as_array()
        .expect("expected stream events");

    assert_eq!(actual, *expected);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let raw_observed = raw_observed.lock().expect("raw stream observations");
    assert_eq!(raw_observed.len(), synthetic["raw_observer_count"]);
    assert_eq!(typed_events.len(), synthetic["typed_event_count"]);
    assert_eq!(raw_observed[0]["cycle"], 3);
    assert_eq!(raw_observed[0]["run_id"], "run_spoofed");
    assert!(typed_events.iter().all(|event| {
        event.run_id() == result.run_id()
            && event.trace_id() == "trace_stream_parity"
            && event.session_id() == Some("session_stream_parity")
            && event.agent_name() == Some("stream-agent")
            && event.cycle_index() == Some(3)
    }));
    assert!(matches!(
        typed_events[0].payload(),
        RunEventPayload::AssistantDelta {
            delta,
            content_chars: Some(4),
            estimated_tokens: Some(1),
        } if delta == "done"
    ));
    assert!(matches!(
        typed_events[1].payload(),
        RunEventPayload::ReasoningDelta {
            delta,
            reasoning_chars: Some(4),
            estimated_tokens: Some(1),
        } if delta == "plan"
    ));
    assert!(matches!(
        typed_events[2].payload(),
        RunEventPayload::ModelToolCallStarted { tool_call_id, .. }
            if tool_call_id == "call_stream"
    ));
    assert!(matches!(
        typed_events[3].payload(),
        RunEventPayload::ModelToolCallProgress { tool_call_id, .. }
            if tool_call_id == "call_stream"
    ));

    let execution_index = result
        .events()
        .iter()
        .position(|event| {
            matches!(
                event.payload(),
                RunEventPayload::ToolCallStarted { tool_call_id, .. }
                    if tool_call_id == "call_stream"
            )
        })
        .expect("executor tool-call start");
    let progress_index = result
        .events()
        .iter()
        .position(|event| std::ptr::eq(event, typed_events[3]))
        .expect("model tool-call progress");
    assert!(execution_index > progress_index);
    assert_eq!(result.final_output(), Some("done"));
    assert_eq!(
        result
            .events()
            .iter()
            .filter(|event| matches!(event.payload(), RunEventPayload::RunCompleted { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn raw_stream_observer_panic_cannot_suppress_typed_journal_or_terminal() {
    let callback_calls = Arc::new(AtomicUsize::new(0));
    let callback_calls_for_config = callback_calls.clone();
    let provider = ContractStreamProvider::default();
    let workspace = tempfile::tempdir().expect("workspace");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(workspace.path())
        .build()
        .expect("runner");
    let agent = Agent::builder("stream-agent")
        .instructions("Finish on the third cycle.")
        .model(ModelRef::named("stream-model"))
        .build()
        .expect("agent");
    let config = RunConfig::builder()
        .max_cycles(3)
        .no_tool_policy(NoToolPolicy::Continue)
        .runtime_stream_callback(move |_| {
            callback_calls_for_config.fetch_add(1, Ordering::SeqCst);
            panic!("raw observer panic");
        })
        .build();

    let result = runner
        .run_with_config(&agent, "stream input", config)
        .await
        .expect("observer panic is isolated");

    assert_eq!(callback_calls.load(Ordering::SeqCst), 5);
    assert_eq!(
        result
            .events()
            .iter()
            .filter(|event| is_typed_stream_event(event.payload()))
            .count(),
        4
    );
    assert_eq!(result.final_output(), Some("done"));
}

#[tokio::test]
async fn malformed_known_stream_events_remain_raw_only() {
    let cases = [
        BTreeMap::from([
            ("event".to_string(), json!("assistant_delta")),
            ("content_delta".to_string(), json!(7)),
        ]),
        BTreeMap::from([
            ("event".to_string(), json!("reasoning_delta")),
            ("reasoning_delta".to_string(), Value::Null),
        ]),
        BTreeMap::from([
            ("event".to_string(), json!("tool_call_started")),
            ("tool_call_id".to_string(), json!("")),
            ("function_name".to_string(), json!("task_finish")),
        ]),
        BTreeMap::from([
            ("event".to_string(), json!("tool_call_progress")),
            ("tool_call_id".to_string(), json!("call_stream")),
            ("function_name".to_string(), json!("task_finish")),
            ("arguments_chars".to_string(), json!(-1)),
        ]),
    ];

    for malformed_event in cases {
        let raw_observer_calls = Arc::new(AtomicUsize::new(0));
        let raw_observer_calls_for_config = raw_observer_calls.clone();
        let provider = ContractStreamProvider {
            client: ContractStreamClient::with_raw_events(vec![malformed_event]),
        };
        let workspace = tempfile::tempdir().expect("workspace");
        let runner = Runner::builder()
            .model_provider(provider)
            .workspace(workspace.path())
            .build()
            .expect("runner");
        let agent = Agent::builder("stream-agent")
            .instructions("Finish on the third cycle.")
            .model(ModelRef::named("stream-model"))
            .build()
            .expect("agent");
        let config = RunConfig::builder()
            .max_cycles(3)
            .no_tool_policy(NoToolPolicy::Continue)
            .runtime_stream_callback(move |_| {
                raw_observer_calls_for_config.fetch_add(1, Ordering::SeqCst);
            })
            .build();

        let result = runner
            .run_with_config(&agent, "stream input", config)
            .await
            .expect("malformed stream cannot fail the run");

        assert_eq!(raw_observer_calls.load(Ordering::SeqCst), 1);
        assert!(!result
            .events()
            .iter()
            .any(|event| is_typed_stream_event(event.payload())));
        assert_eq!(result.final_output(), Some("done"));
    }
}

fn raw_event_map(value: &Value) -> BTreeMap<String, Value> {
    value
        .as_object()
        .expect("raw stream event object")
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn is_typed_stream_event(payload: &RunEventPayload) -> bool {
    matches!(
        payload,
        RunEventPayload::AssistantDelta { .. }
            | RunEventPayload::ReasoningDelta { .. }
            | RunEventPayload::ModelToolCallStarted { .. }
            | RunEventPayload::ModelToolCallProgress { .. }
    )
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
            RunEventPayload::AssistantDelta { delta, .. } => Some(delta.as_str()),
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
    let mut saw_typed_tool_completion = false;
    while let Some(event) = stream.next().await {
        let event = event.expect("event");
        if matches!(
            event.payload(),
            RunEventPayload::ToolCallCompleted { tool_name, .. } if tool_name == "lookup"
        ) {
            assert!(event.has_tool_completion_field("directive"));
            assert!(event.has_tool_completion_field("error_code"));
            assert!(event.has_tool_completion_field("execution_started"));
            assert!(event.has_tool_completion_field("duration_ms"));
            assert_eq!(
                event.tool_directive(),
                Some(vv_agent::ToolDirective::Continue)
            );
            assert_eq!(event.tool_error_code(), None);
            assert_eq!(event.tool_execution_started(), Some(true));
            assert!(event.tool_duration_ms().is_some());
            saw_typed_tool_completion = true;
        }
        let event_type = serde_json::to_value(&event).expect("event JSON")["type"]
            .as_str()
            .expect("event type")
            .to_string();
        if !matches!(event_type.as_str(), "agent_started" | "session_persisted") {
            actual.push(trace_projection(&event));
        }
    }
    let result = handle.result().await.expect("result");
    assert!(saw_typed_tool_completion);
    assert_eq!(
        result.result().cycles[0].tool_results[0].metadata["producer_marker"],
        json!({"nested": true})
    );
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
