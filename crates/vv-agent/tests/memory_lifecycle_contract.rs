use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use vv_agent::memory::{token_utils::count_messages_tokens, CLEARED_MARKER};
use vv_agent::types::AgentTask;
use vv_agent::{
    Agent, AgentRuntime, AgentStatus, ExecutionContext, LLMResponse, LlmClient, LlmError,
    LlmRequest, MemoryCompactMode, MemoryCompactTrigger, MemoryError, MemoryFuture, MemoryManager,
    MemoryManagerConfig, MemoryProvider, MemoryProviderResult, MemorySaveRequest, MemorySaveResult,
    MemorySearchRequest, MemorySearchResult, Message, ModelError, ModelProvider, ModelRef,
    ModelSettings, NoToolPolicy, ReservedOutputSource, ResolvedModelConfig, RunConfig, RunEvent,
    RunEventPayload, Runner, RuntimeRunControls, SessionMemory, SessionMemoryConfig,
    SessionMemoryEntry, ToolCall,
};

#[path = "memory_lifecycle_contract/capacity.rs"]
mod capacity;

fn contract() -> Value {
    serde_json::from_str(include_str!("fixtures/parity/memory_lifecycle.json"))
        .expect("memory lifecycle fixture")
}

fn summary_payload() -> String {
    json!({
        "summary_version": "2.0",
        "original_user_messages": ["original"],
        "user_constraints": [],
        "decisions": [],
        "files_examined_or_modified": [],
        "errors_and_fixes": [],
        "progress": ["done"],
        "key_facts": [],
        "open_issues": [],
        "current_work_state": "done",
        "next_steps": [],
    })
    .to_string()
}

#[tokio::test]
async fn runner_journal_emits_typed_memory_capacity_and_completion_observation() {
    let model_provider = ReusableRecordingModelProvider::default();
    let captured = model_provider.requests.clone();
    let memory_provider = RecordingMemoryProvider::default();
    let runtime_events = Arc::new(Mutex::new(Vec::<RunEvent>::new()));
    let runtime_event_sink = runtime_events.clone();
    let workspace = tempfile::tempdir().expect("workspace");
    let runner = Runner::builder()
        .model_provider(model_provider)
        .workspace(workspace.path())
        .build()
        .expect("runner");
    let agent = Agent::builder("capacity-agent")
        .instructions("Finish.")
        .model(ModelRef::named("capacity-model"))
        .metadata("model_context_window", json!(0))
        .build()
        .expect("agent");

    let result = runner
        .run_with_config(
            &agent,
            "finish",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .initial_messages(vec![
                    Message::system("system"),
                    Message::user("token ".repeat(30_000)),
                    Message::assistant("working"),
                ])
                .memory_provider(Arc::new(memory_provider.clone()))
                .stream(move |event| {
                    if matches!(
                        event.payload(),
                        RunEventPayload::MemoryCompactStarted { .. }
                            | RunEventPayload::MemoryCompactCompleted { .. }
                    ) {
                        runtime_event_sink
                            .lock()
                            .expect("runtime memory events")
                            .push(event.clone());
                    }
                    if matches!(
                        event.payload(),
                        RunEventPayload::MemoryCompactStarted { .. }
                    ) {
                        panic!("typed memory observer panic");
                    }
                })
                .build(),
        )
        .await
        .expect("runner memory lifecycle");

    let captured = captured.lock().expect("runner requests");
    let request = captured
        .iter()
        .find(|request| request.metadata.get("model_max_output_tokens").is_some())
        .cloned()
        .unwrap_or_else(|| panic!("main model request: {captured:#?}"));
    assert_eq!(request.metadata["model_context_window"], 32_000);
    assert_eq!(request.metadata["model_max_output_tokens"], 8_192);
    assert!(request.metadata.get("reserved_output_tokens").is_none());

    let started = result
        .events()
        .iter()
        .find_map(|event| match event.payload() {
            RunEventPayload::MemoryCompactStarted {
                trigger,
                configured_threshold,
                effective_threshold,
                microcompact_threshold,
                model_context_window,
                model_max_output_tokens,
                reserved_output_tokens,
                reserved_output_source,
                autocompact_buffer_tokens,
                ..
            } => Some((
                trigger,
                configured_threshold,
                effective_threshold,
                microcompact_threshold,
                model_context_window,
                model_max_output_tokens,
                reserved_output_tokens,
                reserved_output_source,
                autocompact_buffer_tokens,
            )),
            _ => None,
        })
        .expect("typed memory started event");
    assert_eq!(*started.0, MemoryCompactTrigger::FullThreshold);
    assert_eq!(*started.1, 250_000);
    assert_eq!(*started.2, 10_808);
    assert_eq!(*started.3, 8_106);
    assert_eq!(*started.4, 32_000);
    assert_eq!(*started.5, Some(8_192));
    assert_eq!(*started.6, 8_192);
    assert_eq!(
        *started.7,
        ReservedOutputSource::FrameworkFallbackCappedByModelCapability
    );
    assert_eq!(*started.8, 13_000);

    let completed = result
        .events()
        .iter()
        .find_map(|event| match event.payload() {
            RunEventPayload::MemoryCompactCompleted { mode, changed, .. } => {
                Some((*mode, *changed))
            }
            _ => None,
        })
        .expect("typed memory completed event");
    assert_eq!(completed.0, MemoryCompactMode::Summary);
    assert!(completed.1);

    let provider_events = memory_provider.events.lock().expect("provider events");
    let runtime_events = runtime_events.lock().expect("runtime memory events");
    for (event_name, is_expected_payload) in [
        (
            "memory_compact_started",
            matches_memory_compact_started as fn(&RunEventPayload) -> bool,
        ),
        (
            "memory_compact_completed",
            matches_memory_compact_completed as fn(&RunEventPayload) -> bool,
        ),
    ] {
        let provider_events_for_type = provider_events
            .iter()
            .filter(|event| is_expected_payload(event.payload()))
            .collect::<Vec<_>>();
        let runtime_payloads_for_type = runtime_events
            .iter()
            .filter(|event| is_expected_payload(event.payload()))
            .collect::<Vec<_>>();
        let runner_events_for_type = result
            .events()
            .iter()
            .filter(|event| is_expected_payload(event.payload()))
            .collect::<Vec<_>>();

        assert!(
            !provider_events_for_type.is_empty(),
            "provider {event_name}"
        );
        assert_eq!(
            runtime_payloads_for_type.len(),
            provider_events_for_type.len(),
            "runtime {event_name} count"
        );
        assert_eq!(
            runner_events_for_type.len(),
            provider_events_for_type.len(),
            "Runner {event_name} count"
        );
        for ((provider_event, runtime_event), runner_event) in provider_events_for_type
            .iter()
            .zip(runtime_payloads_for_type.iter())
            .zip(runner_events_for_type.iter())
        {
            assert_eq!(
                runtime_event.event_id(),
                provider_event.event_id(),
                "{event_name} runtime event must reuse the provider event id"
            );
            assert_eq!(
                runtime_event.created_at(),
                provider_event.created_at(),
                "{event_name} runtime event must reuse the provider timestamp"
            );
            assert_eq!(
                runner_event.event_id(),
                provider_event.event_id(),
                "{event_name} Runner journal must reuse the provider event id"
            );
            assert_eq!(
                runner_event.created_at(),
                provider_event.created_at(),
                "{event_name} Runner journal must reuse the provider timestamp"
            );
        }
    }
}

fn matches_memory_compact_started(payload: &RunEventPayload) -> bool {
    matches!(payload, RunEventPayload::MemoryCompactStarted { .. })
}

fn matches_memory_compact_completed(payload: &RunEventPayload) -> bool {
    matches!(payload, RunEventPayload::MemoryCompactCompleted { .. })
}

#[derive(Clone, Default)]
struct ReusableRecordingLlm {
    requests: Arc<Mutex<Vec<LlmRequest>>>,
}

impl LlmClient for ReusableRecordingLlm {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        self.requests.lock().expect("runner requests").push(request);
        Ok(LLMResponse::new("done"))
    }
}

#[derive(Clone, Default)]
struct ReusableRecordingModelProvider {
    requests: Arc<Mutex<Vec<LlmRequest>>>,
}

impl ModelProvider for ReusableRecordingModelProvider {
    fn resolve(&self, model: &ModelRef) -> Result<ResolvedModelConfig, ModelError> {
        let model = model.model();
        Ok(
            ResolvedModelConfig::new("scripted", model, model, model, Vec::new())
                .with_token_limits(Some(32_000), Some(8_192))
                .with_capabilities(true, true, false),
        )
    }

    fn client(&self, _resolved: &ResolvedModelConfig) -> Result<Arc<dyn LlmClient>, ModelError> {
        Ok(Arc::new(ReusableRecordingLlm {
            requests: self.requests.clone(),
        }))
    }
}

#[derive(Clone, Default)]
struct OneShotLlm;

impl LlmClient for OneShotLlm {
    fn complete(&self, _request: LlmRequest) -> Result<LLMResponse, LlmError> {
        Ok(LLMResponse::new("done"))
    }
}

#[derive(Clone, Default)]
struct SummaryLlm {
    requests: Arc<Mutex<Vec<LlmRequest>>>,
    response: Arc<Mutex<Option<String>>>,
}

impl SummaryLlm {
    fn responding_with(response: impl Into<String>) -> Self {
        Self {
            response: Arc::new(Mutex::new(Some(response.into()))),
            ..Self::default()
        }
    }
}

impl LlmClient for SummaryLlm {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        self.requests
            .lock()
            .expect("summary requests")
            .push(request);
        let response = self
            .response
            .lock()
            .expect("summary response")
            .clone()
            .unwrap_or_else(summary_payload);
        Ok(LLMResponse::new(response))
    }
}

#[derive(Clone, Default)]
struct RecordingModelProvider {
    resolutions: Arc<Mutex<Vec<(String, String)>>>,
    summary_llm: SummaryLlm,
}

impl RecordingModelProvider {
    fn responding_with(response: impl Into<String>) -> Self {
        Self {
            summary_llm: SummaryLlm::responding_with(response),
            ..Self::default()
        }
    }
}

impl ModelProvider for RecordingModelProvider {
    fn resolve(&self, model: &ModelRef) -> Result<ResolvedModelConfig, ModelError> {
        let backend = model
            .backend_name()
            .ok_or_else(|| ModelError::Config("missing backend".to_string()))?;
        let model_name = model.model();
        self.resolutions
            .lock()
            .expect("model resolutions")
            .push((backend.to_string(), model_name.to_string()));
        Ok(ResolvedModelConfig::new(
            backend,
            model_name,
            model_name,
            model_name,
            Vec::new(),
        ))
    }

    fn client(&self, _resolved: &ResolvedModelConfig) -> Result<Arc<dyn LlmClient>, ModelError> {
        Ok(Arc::new(self.summary_llm.clone()))
    }
}

#[test]
fn runtime_routes_summary_through_configured_backend_model_pair() {
    let contract = contract();
    let route = &contract["summary_route"];
    let provider = RecordingModelProvider::default();
    let inspector = provider.clone();
    let workspace = tempfile::tempdir().expect("workspace");
    let mut runtime = AgentRuntime::new(OneShotLlm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    let mut task = AgentTask::new("memory_route", "main-model", "system", "continue");
    task.initial_messages = vec![
        Message::system("system"),
        Message::user("u".repeat(160)),
        Message::assistant("a".repeat(160)),
        Message::user("c".repeat(160)),
    ];
    task.max_cycles = 1;
    task.no_tool_policy = NoToolPolicy::Finish;
    task.memory_compact_threshold = 40;
    task.metadata.insert(
        "memory_summary_backend".to_string(),
        route["backend"].clone(),
    );
    task.metadata
        .insert("memory_summary_model".to_string(), route["model"].clone());
    task.metadata
        .insert("model_context_window".to_string(), json!(60));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(10));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(10));
    task.metadata
        .insert("session_memory_enabled".to_string(), json!(false));

    let result = runtime
        .run_with_controls(
            task,
            RuntimeRunControls {
                model_provider: Some(Arc::new(provider)),
                workspace: Some(workspace.path().to_path_buf()),
                ..RuntimeRunControls::default()
            },
        )
        .expect("runtime result");

    assert_eq!(result.status, AgentStatus::Completed);
    let resolutions = inspector.resolutions.lock().expect("model resolutions");
    assert_eq!(
        resolutions.as_slice(),
        &[(
            route["backend"].as_str().expect("backend").to_string(),
            route["model"].as_str().expect("model").to_string(),
        )]
    );
    assert_eq!(
        resolutions.len() as u64,
        route["resolution_count"]
            .as_u64()
            .expect("resolution count")
    );
    let requests = inspector
        .summary_llm
        .requests
        .lock()
        .expect("summary requests");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].model, route["request_model"].as_str().unwrap());
}

#[test]
fn runtime_routes_session_extraction_through_its_own_backend_model_pair() {
    let contract = contract();
    let route = &contract["session_extraction_route"];
    let provider = RecordingModelProvider::responding_with(
        r#"[{"category":"decision","content":"route extraction separately","importance":8}]"#,
    );
    let inspector = provider.clone();
    let main_llm = SummaryLlm::responding_with("done");
    let main_inspector = main_llm.clone();
    let workspace = tempfile::tempdir().expect("workspace");
    let mut runtime = AgentRuntime::new(main_llm);
    runtime.default_workspace = Some(workspace.path().to_path_buf());
    let mut task = AgentTask::new(
        "session_memory_route",
        "main-model",
        "system",
        "remember this decision",
    );
    task.max_cycles = 1;
    task.no_tool_policy = NoToolPolicy::Finish;
    task.memory_compact_threshold = 10_000;
    task.metadata
        .insert("session_memory_enabled".to_string(), json!(true));
    task.metadata
        .insert("session_memory_min_tokens".to_string(), json!(1));
    task.metadata
        .insert("session_memory_min_text_messages".to_string(), json!(1));
    task.metadata.insert(
        "session_memory_extraction_backend".to_string(),
        route["backend"].clone(),
    );
    task.metadata.insert(
        "session_memory_extraction_model".to_string(),
        route["model"].clone(),
    );
    task.metadata
        .insert("model_context_window".to_string(), json!(20_000));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(0));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));

    let result = runtime
        .run_with_controls(
            task,
            RuntimeRunControls {
                model_provider: Some(Arc::new(provider)),
                workspace: Some(workspace.path().to_path_buf()),
                ..RuntimeRunControls::default()
            },
        )
        .expect("runtime result");

    assert_eq!(result.status, AgentStatus::Completed);
    let resolutions = inspector.resolutions.lock().expect("model resolutions");
    assert_eq!(
        resolutions.as_slice(),
        &[(
            route["backend"].as_str().expect("backend").to_string(),
            route["model"].as_str().expect("model").to_string(),
        )]
    );
    assert_eq!(
        resolutions.len() as u64,
        route["resolution_count"].as_u64().unwrap()
    );
    let extraction_requests = inspector
        .summary_llm
        .requests
        .lock()
        .expect("extraction requests");
    assert_eq!(extraction_requests.len(), 1);
    assert_eq!(
        extraction_requests[0].model,
        route["request_model"].as_str().unwrap()
    );
    let main_requests = main_inspector.requests.lock().expect("main requests");
    assert_eq!(main_requests.len(), 1);
    assert!(main_requests[0].messages[0]
        .content
        .contains("route extraction separately"));
}

#[derive(Clone, Default)]
struct RecordingMemoryProvider {
    calls: Arc<Mutex<Vec<String>>>,
    events: Arc<Mutex<Vec<RunEvent>>>,
    fail_before: bool,
    fail_after: bool,
}

impl RecordingMemoryProvider {
    fn failing() -> Self {
        Self {
            fail_before: true,
            fail_after: true,
            ..Self::default()
        }
    }
}

impl MemoryProvider for RecordingMemoryProvider {
    fn search(&self, _request: MemorySearchRequest) -> MemoryFuture<Vec<MemorySearchResult>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn save(&self, _request: MemorySaveRequest) -> MemoryFuture<MemorySaveResult> {
        Box::pin(async { Ok(MemorySaveResult::default()) })
    }

    fn before_compact(&self, event: &RunEvent) -> MemoryFuture<MemoryProviderResult> {
        assert!(event.metadata().contains_key("messages"));
        self.calls
            .lock()
            .expect("provider calls")
            .push("before".to_string());
        self.events
            .lock()
            .expect("provider events")
            .push(event.clone());
        let fail = self.fail_before;
        Box::pin(async move {
            if fail {
                return Err(MemoryError::new("before exploded"));
            }
            Ok(MemoryProviderResult {
                metadata: BTreeMap::from([(
                    "phase".to_string(),
                    Value::String("before".to_string()),
                )]),
            })
        })
    }

    fn after_compact(&self, event: &RunEvent) -> MemoryFuture<()> {
        self.calls
            .lock()
            .expect("provider calls")
            .push("after".to_string());
        self.events
            .lock()
            .expect("provider events")
            .push(event.clone());
        let fail = self.fail_after;
        Box::pin(async move {
            if fail {
                return Err(MemoryError::new("after exploded"));
            }
            Ok(())
        })
    }
}

#[derive(Clone, Default)]
struct PromptTooLongThenSuccess {
    requests: Arc<Mutex<usize>>,
    failures: usize,
}

impl PromptTooLongThenSuccess {
    fn new(failures: usize) -> Self {
        Self {
            failures,
            ..Self::default()
        }
    }
}

impl LlmClient for PromptTooLongThenSuccess {
    fn complete(&self, _request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let mut requests = self.requests.lock().expect("request count");
        *requests += 1;
        if *requests <= self.failures {
            return Err(LlmError::Request(
                "Prompt is too long for this model".to_string(),
            ));
        }
        Ok(LLMResponse::new("done"))
    }
}

fn ptl_task() -> AgentTask {
    let mut task = AgentTask::new("memory_ptl", "main-model", "system", "continue");
    task.initial_messages = vec![
        Message::system("system"),
        Message::user("first"),
        Message::assistant("working"),
    ];
    task.no_tool_policy = NoToolPolicy::Finish;
    task.memory_compact_threshold = 10_000;
    task.metadata
        .insert("model_context_window".to_string(), json!(20_000));
    task.metadata
        .insert("reserved_output_tokens".to_string(), json!(0));
    task.metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));
    task
}

#[test]
fn ptl_forced_and_emergency_attempts_notify_providers() {
    let contract = contract();
    let attempts = &contract["provider_attempts"];
    let provider = RecordingMemoryProvider::default();
    let logs = Arc::new(Mutex::new(Vec::<RunEvent>::new()));
    let log_sink = logs.clone();
    let runtime = AgentRuntime::new(PromptTooLongThenSuccess::new(
        attempts["prompt_too_long_failures_before_success"]
            .as_u64()
            .expect("failure count") as usize,
    ));

    let result = runtime
        .run_with_controls(
            ptl_task(),
            RuntimeRunControls {
                execution_context: Some(ExecutionContext {
                    memory_providers: vec![Arc::new(provider.clone())],
                    metadata: BTreeMap::from([
                        ("_vv_agent_run_id".to_string(), json!("run_memory")),
                        ("_vv_agent_trace_id".to_string(), json!("trace_memory")),
                        ("_vv_agent_agent_name".to_string(), json!("assistant")),
                    ]),
                    ..ExecutionContext::default()
                }),
                event_handler: Some(Arc::new(move |event| {
                    if matches!(
                        event.payload(),
                        RunEventPayload::MemoryCompactStarted { .. }
                            | RunEventPayload::MemoryCompactCompleted { .. }
                    ) {
                        log_sink.lock().expect("memory logs").push(event.clone());
                    }
                })),
                ..RuntimeRunControls::default()
            },
        )
        .expect("runtime result");

    assert_eq!(result.status, AgentStatus::Completed);
    assert!(result.cycles[0].memory_compacted);
    assert_eq!(
        provider.calls.lock().expect("provider calls").as_slice(),
        &[
            "before".to_string(),
            "after".to_string(),
            "before".to_string(),
            "after".to_string(),
        ]
    );
    let memory_logs = logs.lock().expect("memory logs").clone();
    assert_eq!(
        memory_logs
            .iter()
            .filter(|event| matches!(
                event.payload(),
                RunEventPayload::MemoryCompactStarted { .. }
            ))
            .count() as u64,
        attempts["started_count"].as_u64().unwrap()
    );
    assert_eq!(
        memory_logs
            .iter()
            .filter(|event| matches!(
                event.payload(),
                RunEventPayload::MemoryCompactCompleted { .. }
            ))
            .count() as u64,
        attempts["completed_count"].as_u64().unwrap()
    );
    assert_eq!(
        memory_logs[0].metadata()["memory_provider_results"]["RecordingMemoryProvider"]["phase"],
        attempts["result_metadata"]["phase"]
    );
    let started = memory_logs
        .iter()
        .filter(|event| {
            matches!(
                event.payload(),
                RunEventPayload::MemoryCompactStarted { .. }
            )
        })
        .collect::<Vec<_>>();
    assert!(started.iter().all(|event| matches!(
        event.payload(),
        RunEventPayload::MemoryCompactStarted {
            trigger: MemoryCompactTrigger::PromptTooLong,
            ..
        }
    )));
    assert!(started.iter().all(|event| {
        let payload = serde_json::to_value(event).expect("started event wire");
        contract["compaction_events"]["started"]["new_producer_fields"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .all(|field| payload.get(field).is_some())
    }));
    let completed = memory_logs
        .iter()
        .filter(|event| {
            matches!(
                event.payload(),
                RunEventPayload::MemoryCompactCompleted { .. }
            )
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        completed[0].payload(),
        RunEventPayload::MemoryCompactCompleted {
            mode: MemoryCompactMode::Summary,
            changed: true,
            ..
        }
    ));
    assert!(matches!(
        completed[1].payload(),
        RunEventPayload::MemoryCompactCompleted {
            mode: MemoryCompactMode::None,
            changed: false,
            ..
        }
    ));
}

#[test]
fn memory_provider_attempt_errors_are_fail_open() {
    let contract = contract();
    let attempts = &contract["provider_attempts"];
    let provider = RecordingMemoryProvider::failing();
    let logs = Arc::new(Mutex::new(Vec::<RunEvent>::new()));
    let log_sink = logs.clone();
    let runtime = AgentRuntime::new(PromptTooLongThenSuccess::new(1));

    let result = runtime
        .run_with_controls(
            ptl_task(),
            RuntimeRunControls {
                execution_context: Some(ExecutionContext {
                    memory_providers: vec![Arc::new(provider)],
                    ..ExecutionContext::default()
                }),
                event_handler: Some(Arc::new(move |event| {
                    if matches!(
                        event.payload(),
                        RunEventPayload::MemoryCompactStarted { .. }
                            | RunEventPayload::MemoryCompactCompleted { .. }
                    ) {
                        log_sink.lock().expect("memory logs").push(event.clone());
                    }
                })),
                ..RuntimeRunControls::default()
            },
        )
        .expect("runtime result");

    assert_eq!(result.status, AgentStatus::Completed);
    let logs = logs.lock().expect("memory logs");
    let started = logs
        .iter()
        .find(|event| {
            matches!(
                event.payload(),
                RunEventPayload::MemoryCompactStarted { .. }
            )
        })
        .expect("started event");
    let completed = logs
        .iter()
        .find(|event| {
            matches!(
                event.payload(),
                RunEventPayload::MemoryCompactCompleted { .. }
            )
        })
        .expect("completed event");
    assert_eq!(
        started.metadata()["memory_provider_errors"][0]["stage"],
        attempts["before_error"]["stage"]
    );
    assert_eq!(
        started.metadata()["memory_provider_errors"][0]["error"],
        attempts["before_error"]["error"]
    );
    assert_eq!(
        completed.metadata()["memory_provider_errors"][0]["stage"],
        attempts["after_error"]["stage"]
    );
    assert_eq!(
        completed.metadata()["memory_provider_errors"][0]["error"],
        attempts["after_error"]["error"]
    );
}

#[test]
fn session_memory_refreshes_in_place_and_resets_token_baseline() {
    let contract = contract();
    let expected = &contract["session_memory"];
    let mut session_memory = SessionMemory::new(SessionMemoryConfig::default());
    session_memory.state.entries = vec![SessionMemoryEntry::new(
        "decision",
        expected["stale_fact"].as_str().unwrap(),
        1,
        5,
    )];
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        compact_threshold: 40,
        model: "main-model".to_string(),
        model_context_window: 60,
        reserved_output_tokens: 10,
        autocompact_buffer_tokens: 10,
        session_memory: Some(session_memory),
        ..MemoryManagerConfig::default()
    });
    let messages = vec![
        Message::system("system"),
        Message::user("u".repeat(120)),
        Message::assistant("a".repeat(120)),
        Message::user("c".repeat(120)),
    ];
    let stale = manager.apply_session_memory_context(&messages);
    manager
        .session_memory_mut()
        .expect("session memory")
        .state
        .entries = vec![SessionMemoryEntry::new(
        "decision",
        expected["fresh_fact"].as_str().unwrap(),
        2,
        5,
    )];

    let refreshed = manager.apply_session_memory_context(&stale);

    assert!(!refreshed[0]
        .content
        .contains(expected["stale_fact"].as_str().unwrap()));
    assert!(refreshed[0]
        .content
        .contains(expected["fresh_fact"].as_str().unwrap()));
    assert_eq!(
        refreshed[0].content.matches("<Session Memory>").count() as u64,
        expected["block_count"].as_u64().unwrap()
    );

    let (compacted, changed) = manager.compact_for_cycle(&refreshed, 3, true);
    let reinjected = manager.apply_session_memory_context(&compacted);
    let expected_baseline = count_messages_tokens(&reinjected, "main-model");
    let compacted_tokens = count_messages_tokens(&compacted, "main-model");
    let state = &manager.session_memory().expect("session memory").state;

    assert!(changed);
    assert_eq!(
        state.initialized,
        expected["initialized_after_compaction"].as_bool().unwrap()
    );
    assert_eq!(state.tokens_at_last_extraction, expected_baseline);
    assert!(expected_baseline > compacted_tokens);
}

fn fallback_name_matches(name: &str) -> bool {
    let Some(hex) = name
        .strip_prefix("tool_result_")
        .and_then(|name| name.strip_suffix(".txt"))
    else {
        return false;
    };
    hex.len() == 32 && hex.chars().all(|character| character.is_ascii_hexdigit())
}

fn artifact_messages(tool_call_id: &str, contents: &[&str]) -> Vec<Message> {
    let calls = contents
        .iter()
        .map(|_| ToolCall::new(tool_call_id, "read_file", BTreeMap::new()))
        .collect::<Vec<_>>();
    let mut messages = vec![
        Message::system("system"),
        Message::user("read files"),
        Message {
            tool_calls: calls,
            ..Message::assistant("reading")
        },
    ];
    messages.extend(
        contents
            .iter()
            .map(|content| Message::tool(*content, tool_call_id)),
    );
    messages.push(Message::assistant("continue"));
    messages
}

fn compact_artifacts(
    workspace: &Path,
    artifact_dir: &str,
    tool_call_id: &str,
    contents: &[&str],
) -> Vec<Message> {
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        compact_threshold: 10,
        model_context_window: 80,
        reserved_output_tokens: 10,
        autocompact_buffer_tokens: 0,
        tool_result_compact_threshold: 10,
        tool_result_keep_last: 0,
        tool_result_artifact_dir: artifact_dir.into(),
        workspace: Some(workspace.to_path_buf()),
        ..MemoryManagerConfig::default()
    });
    manager
        .compact_for_cycle(&artifact_messages(tool_call_id, contents), 4, false)
        .0
}

#[test]
fn artifact_fallbacks_are_unique_and_fail_open_at_workspace_boundary() {
    let contract = contract();
    let expected = &contract["artifacts"];
    assert_eq!(
        expected["fallback_pattern"].as_str().unwrap(),
        "^tool_result_[0-9a-f]{32}\\.txt$"
    );
    let root = tempfile::tempdir().expect("test root");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace");
    let compacted = compact_artifacts(
        &workspace,
        ".memory/tool_results",
        "/",
        &["first artifact payload", "second artifact payload"],
    );
    assert!(compacted
        .iter()
        .any(|message| message.content.contains("<Persisted Artifacts>")));
    let artifact_dir = workspace.join(".memory/tool_results/cycle_4");
    let mut fallback_names = std::fs::read_dir(&artifact_dir)
        .expect("artifact directory")
        .map(|entry| {
            entry
                .expect("artifact entry")
                .file_name()
                .to_string_lossy()
                .to_string()
        })
        .collect::<Vec<_>>();
    fallback_names.sort();
    assert_eq!(
        fallback_names.len() as u64,
        expected["fallback_count"].as_u64().unwrap()
    );
    assert!(fallback_names
        .iter()
        .all(|name| fallback_name_matches(name)));
    assert_ne!(fallback_names[0], fallback_names[1]);

    let blocked = workspace.join("blocked");
    std::fs::write(&blocked, "not a directory").expect("blocked path");
    let failed = compact_artifacts(
        &workspace,
        "blocked/nested",
        "call",
        &["write failure payload"],
    );
    assert!(failed
        .iter()
        .any(|message| message.content.contains("artifact_path: N/A")));
    assert_eq!(expected["write_failure_path"].as_str().unwrap(), "N/A");

    let escaped = compact_artifacts(&workspace, "../outside", "call", &["escape payload"]);
    assert!(escaped
        .iter()
        .any(|message| message.content.contains("artifact_path: N/A")));
    assert_eq!(expected["escape_path"].as_str().unwrap(), "N/A");
    assert!(!root.path().join("outside").exists());
}
