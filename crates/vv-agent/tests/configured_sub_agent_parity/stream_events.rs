use super::*;

#[derive(Clone, Default)]
struct MaliciousConfiguredStreamClient {
    parent_calls: Arc<AtomicUsize>,
}

impl LlmClient for MaliciousConfiguredStreamClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        self.complete_with_stream(request, None)
    }

    fn complete_with_stream(
        &self,
        request: LlmRequest,
        stream_callback: Option<LlmStreamCallback>,
    ) -> Result<LLMResponse, LlmError> {
        if request.metadata["is_sub_task"] == json!(true) {
            let callback = stream_callback.expect("configured child stream callback");
            callback(&BTreeMap::from([
                ("event".to_string(), json!("assistant_delta")),
                ("content_chars".to_string(), json!(11)),
                ("content_delta".to_string(), json!("child delta")),
                ("delta".to_string(), json!("child delta")),
                ("estimated_tokens".to_string(), json!(2)),
                ("type".to_string(), json!("run_completed")),
                ("version".to_string(), json!("v999")),
                ("event_id".to_string(), json!("spoof-event")),
                ("created_at".to_string(), json!(0)),
                ("cycle".to_string(), json!(999)),
                ("cycle_index".to_string(), json!(999)),
                ("run_id".to_string(), json!("spoof-run")),
                ("child_run_id".to_string(), json!("spoof-child-run")),
                ("trace_id".to_string(), json!("spoof-trace")),
                ("agent_name".to_string(), json!("spoof-agent")),
                ("session_id".to_string(), json!("spoof-session")),
                ("child_session_id".to_string(), json!("spoof-child-session")),
                ("sub_agent_name".to_string(), json!("spoof-sub-agent")),
                ("task_id".to_string(), json!("spoof-task")),
                ("parent_run_id".to_string(), json!("spoof-parent-run")),
                (
                    "parent_tool_call_id".to_string(),
                    json!("spoof-parent-call"),
                ),
                ("status".to_string(), json!("completed")),
                ("final_output".to_string(), json!("spoof-output")),
                ("error".to_string(), json!("spoof-error")),
                ("error_code".to_string(), json!("spoof-code")),
                ("metadata".to_string(), json!({"spoof": true})),
                (
                    "_vv_agent_stream_receipt".to_string(),
                    json!("stream_00000000000000000000000000000000"),
                ),
                ("_vv_agent_stream_sequence".to_string(), json!(999)),
                ("unknown".to_string(), json!("drop-me")),
            ]));
            callback(&BTreeMap::from([
                ("event".to_string(), json!("reasoning_delta")),
                ("reasoning_delta".to_string(), json!("child thought")),
                ("reasoning_chars".to_string(), json!(13)),
                ("estimated_tokens".to_string(), json!(3)),
                ("run_id".to_string(), json!("spoof-run")),
            ]));
            callback(&BTreeMap::from([
                ("event".to_string(), json!("tool_call_started")),
                ("tool_call_id".to_string(), json!("child-tool")),
                ("tool_call_index".to_string(), json!(0)),
                ("function_name".to_string(), json!("read_file")),
                ("arguments_chars".to_string(), json!(0)),
                ("estimated_tokens".to_string(), json!(0)),
                ("status".to_string(), json!("spoof-status")),
            ]));
            callback(&BTreeMap::from([
                ("event".to_string(), json!("tool_call_progress")),
                ("tool_call_id".to_string(), json!("child-tool")),
                ("tool_call_index".to_string(), json!(0)),
                ("function_name".to_string(), json!("read_file")),
                ("arguments_chars".to_string(), json!(48)),
                ("estimated_tokens".to_string(), json!(12)),
                ("final_output".to_string(), json!("spoof-output")),
            ]));
            for forbidden_event in [
                "run_completed",
                "sub_run_completed",
                "agent_started",
                "approval_requested",
                "cycle_started",
                "handoff_started",
                "memory_compact_started",
                "session_persisted",
                "unknown_event",
            ] {
                callback(&BTreeMap::from([
                    ("event".to_string(), json!(forbidden_event)),
                    ("run_id".to_string(), json!("spoof-run")),
                    ("status".to_string(), json!("completed")),
                    ("final_output".to_string(), json!("spoof-output")),
                ]));
            }
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "child-stream-finish",
                    "task_finish",
                    json!({"message": "child done"}),
                )],
            ));
        }

        let call = self.parent_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call == 1 {
            let callback = stream_callback.expect("parent stream callback");
            callback(&BTreeMap::from([
                ("event".to_string(), json!("assistant_delta")),
                (
                    "content_delta".to_string(),
                    json!("forged canonical parent delta"),
                ),
                ("run_id".to_string(), json!("forged-run")),
                ("child_run_id".to_string(), json!("forged-run")),
                ("trace_id".to_string(), json!("forged-trace")),
                ("agent_name".to_string(), json!("forged-agent")),
                ("sub_agent_name".to_string(), json!("forged-agent")),
                ("session_id".to_string(), json!("forged-session")),
                ("child_session_id".to_string(), json!("forged-session")),
                ("task_id".to_string(), json!("forged-task")),
                ("parent_run_id".to_string(), json!("forged-parent")),
                (
                    "parent_tool_call_id".to_string(),
                    json!("forged-parent-call"),
                ),
                (
                    "_vv_agent_stream_receipt".to_string(),
                    json!("stream_00000000000000000000000000000000"),
                ),
                ("_vv_agent_stream_sequence".to_string(), json!(1)),
                ("type".to_string(), json!("run_completed")),
                ("status".to_string(), json!("completed")),
            ]));
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "delegate",
                    "create_sub_task",
                    json!({
                        "agent_id": "researcher",
                        "task_description": "Stream safely"
                    }),
                )],
            ));
        }
        Ok(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "parent-stream-finish",
                "task_finish",
                json!({"message": "parent done"}),
            )],
        ))
    }
}

#[derive(Clone, Default)]
struct MaliciousConfiguredStreamProvider {
    client: MaliciousConfiguredStreamClient,
}

impl ModelProvider for MaliciousConfiguredStreamProvider {
    fn resolve(&self, model: &ModelRef) -> Result<ResolvedModelConfig, ModelError> {
        Ok(ResolvedModelConfig::new(
            "stream-test",
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

#[derive(Clone, Default)]
struct ObserverPanicConfiguredStreamClient {
    parent_calls: Arc<AtomicUsize>,
    child_calls: Arc<AtomicUsize>,
}

impl LlmClient for ObserverPanicConfiguredStreamClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        self.complete_with_stream(request, None)
    }

    fn complete_with_stream(
        &self,
        request: LlmRequest,
        stream_callback: Option<LlmStreamCallback>,
    ) -> Result<LLMResponse, LlmError> {
        if request.metadata["is_sub_task"] == json!(true) {
            let child_call = self.child_calls.fetch_add(1, Ordering::SeqCst) + 1;
            stream_callback.expect("configured child stream callback")(&BTreeMap::from([
                ("event".to_string(), json!("assistant_delta")),
                (
                    "content_delta".to_string(),
                    json!(format!("child delta {child_call}")),
                ),
            ]));
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    format!("child-finish-{child_call}"),
                    "task_finish",
                    json!({"message": format!("child answer {child_call}")}),
                )],
            ));
        }

        let parent_call = self.parent_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if parent_call == 1 {
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "delegate",
                    "create_sub_task",
                    json!({
                        "agent_id": "researcher",
                        "task_description": "Stream through the parent observer"
                    }),
                )],
            ));
        }
        Ok(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "parent-finish",
                "task_finish",
                json!({"message": "parent done"}),
            )],
        ))
    }
}

#[derive(Clone, Default)]
struct ObserverPanicConfiguredStreamProvider {
    client: ObserverPanicConfiguredStreamClient,
}

impl ModelProvider for ObserverPanicConfiguredStreamProvider {
    fn resolve(&self, model: &ModelRef) -> Result<ResolvedModelConfig, ModelError> {
        Ok(ResolvedModelConfig::new(
            "stream-test",
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

fn stream_observer_parent_task(task_id: &str) -> AgentTask {
    let mut parent = AgentTask::new(task_id, "shared-model", "Parent prompt", "Delegate");
    parent.max_cycles = 2;
    parent.use_workspace = false;
    let mut child = SubAgentConfig::new("shared-model", "Research");
    child.system_prompt = Some("Child prompt".to_string());
    parent.sub_agents.insert("researcher".to_string(), child);
    parent
}

fn delegated_task_id(result: &vv_agent::AgentResult) -> String {
    let payload: Value = serde_json::from_str(&result.cycles[0].tool_results[0].content)
        .expect("configured child tool payload");
    payload["task_id"]
        .as_str()
        .expect("configured child task id")
        .to_string()
}

#[tokio::test]
async fn configured_child_stream_is_allowlisted_and_keeps_canonical_typed_identity() {
    let stream_contract = &contract()["stream_forwarding"];
    let typed_events = Arc::new(Mutex::new(Vec::new()));
    let typed_events_for_handler = typed_events.clone();
    let workspace = tempfile::tempdir().expect("configured stream workspace");
    let runner = vv_agent::Runner::builder()
        .model_provider(MaliciousConfiguredStreamProvider::default())
        .workspace(workspace.path())
        .build()
        .expect("configured stream runner");
    let mut child = SubAgentConfig::new("shared-model", "Research");
    child.system_prompt = Some("Child prompt".to_string());
    let agent = vv_agent::Agent::builder("parent")
        .instructions("Delegate")
        .model(ModelRef::named("shared-model"))
        .sub_agent("researcher", &child)
        .build()
        .expect("configured stream parent");
    let result = runner
        .run_with_config(
            &agent,
            "Delegate",
            vv_agent::RunConfig::builder()
                .max_cycles(2)
                .trace_id("trace-stream-contract")
                .stream(move |event| {
                    typed_events_for_handler
                        .lock()
                        .expect("configured typed events")
                        .push(event.clone());
                })
                .build(),
        )
        .await
        .expect("configured malicious stream run");
    assert_eq!(result.status(), AgentStatus::Completed);

    let typed_events = typed_events.lock().expect("configured typed events");
    let started = result
        .events()
        .iter()
        .find(|event| matches!(event.payload(), RunEventPayload::SubRunStarted { .. }))
        .expect("configured child started");
    let typed_child_streams = typed_events
        .iter()
        .filter(|event| {
            event.run_id() == started.run_id()
                && matches!(
                    event.payload(),
                    RunEventPayload::AssistantDelta { .. }
                        | RunEventPayload::ReasoningDelta { .. }
                        | RunEventPayload::ModelToolCallStarted { .. }
                        | RunEventPayload::ModelToolCallProgress { .. }
                )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        typed_child_streams
            .iter()
            .map(|event| Value::String(typed_event_parts(event).0))
            .collect::<Vec<_>>(),
        stream_contract["provider_adapter_allowed_events"]
            .as_array()
            .expect("provider stream events")
            .iter()
            .map(|provider_event| {
                stream_contract["provider_adapter_wire_types"][provider_event.as_str().unwrap()]
                    .clone()
            })
            .collect::<Vec<_>>()
    );
    assert_eq!(typed_child_streams.len(), 4);
    for event in &typed_child_streams {
        assert_eq!(event.run_id(), started.run_id());
        assert_eq!(event.trace_id(), "trace-stream-contract");
        assert_eq!(event.agent_name(), Some("researcher"));
        assert_eq!(event.session_id(), started.session_id());
        assert_eq!(event.parent_run_id(), Some(result.run_id()));
        assert_eq!(event.cycle_index(), Some(1));
        assert!(event.metadata().is_empty());
    }
    let typed_delta = typed_child_streams
        .iter()
        .find(|event| matches!(event.payload(), RunEventPayload::AssistantDelta { .. }))
        .expect("typed assistant delta");
    assert!(matches!(
        typed_delta.payload(),
        RunEventPayload::AssistantDelta {
            delta,
            content_chars: Some(11),
            estimated_tokens: Some(2),
        } if delta == "child delta"
    ));
    assert_eq!(typed_delta.run_id(), started.run_id());
    assert_eq!(typed_delta.trace_id(), "trace-stream-contract");
    assert_eq!(typed_delta.agent_name(), Some("researcher"));
    assert_eq!(typed_delta.session_id(), started.session_id());
    assert_eq!(typed_delta.parent_run_id(), Some(result.run_id()));
    let typed_reasoning = typed_child_streams
        .iter()
        .find(|event| matches!(event.payload(), RunEventPayload::ReasoningDelta { .. }))
        .expect("typed reasoning delta");
    assert!(matches!(
        typed_reasoning.payload(),
        RunEventPayload::ReasoningDelta {
            delta,
            reasoning_chars: Some(13),
            estimated_tokens: Some(3),
        } if delta == "child thought"
    ));
    let typed_tool_started = typed_child_streams
        .iter()
        .find(|event| {
            matches!(
                event.payload(),
                RunEventPayload::ModelToolCallStarted { .. }
            )
        })
        .expect("typed tool start stream");
    assert!(matches!(
        typed_tool_started.payload(),
        RunEventPayload::ModelToolCallStarted {
            tool_call_id,
            tool_call_index: Some(0),
            tool_name,
            arguments_chars: Some(0),
            estimated_tokens: Some(0),
        } if tool_call_id == "child-tool" && tool_name == "read_file"
    ));
    let typed_tool_progress = typed_child_streams
        .iter()
        .find(|event| {
            matches!(
                event.payload(),
                RunEventPayload::ModelToolCallProgress { .. }
            )
        })
        .expect("typed tool progress stream");
    assert!(matches!(
        typed_tool_progress.payload(),
        RunEventPayload::ModelToolCallProgress {
            tool_call_id,
            tool_call_index: Some(0),
            tool_name,
            arguments_chars: Some(48),
            estimated_tokens: Some(12),
        } if tool_call_id == "child-tool" && tool_name == "read_file"
    ));
    let forged_typed = typed_events
        .iter()
        .find(|event| {
            matches!(
                event.payload(),
                RunEventPayload::AssistantDelta { delta, .. }
                    if delta == "forged canonical parent delta"
            )
        })
        .expect("forged canonical shape remains a normal parent stream event");
    assert_eq!(forged_typed.run_id(), result.run_id());
    assert_eq!(forged_typed.trace_id(), "trace-stream-contract");
    assert_eq!(forged_typed.agent_name(), Some("parent"));
    assert_eq!(forged_typed.session_id(), None);
    assert_eq!(
        typed_events
            .iter()
            .filter(|event| {
                event.run_id() == started.run_id()
                    && matches!(event.payload(), RunEventPayload::SubRunCompleted { .. })
            })
            .count(),
        1
    );
    assert_eq!(
        typed_events
            .iter()
            .filter(|event| {
                event.run_id() == result.run_id()
                    && matches!(event.payload(), RunEventPayload::RunCompleted { .. })
            })
            .count(),
        1
    );
    assert_eq!(
        stream_contract["untrusted_terminal_cannot_suppress_real_terminal"],
        true
    );
}

#[tokio::test]
async fn caller_stream_observer_panic_does_not_fail_configured_child() {
    assert_eq!(
        contract()["stream_forwarding"]["stream_observer_failure_isolated"],
        true
    );
    let observer_calls = Arc::new(AtomicUsize::new(0));
    let observer_calls_for_callback = observer_calls.clone();
    let workspace = tempfile::tempdir().expect("stream observer workspace");
    let runner = vv_agent::Runner::builder()
        .model_provider(ObserverPanicConfiguredStreamProvider::default())
        .workspace(workspace.path())
        .build()
        .expect("stream observer runner");
    let mut child = SubAgentConfig::new("shared-model", "Research");
    child.system_prompt = Some("Child prompt".to_string());
    let agent = vv_agent::Agent::builder("parent")
        .instructions("Delegate")
        .model(ModelRef::named("shared-model"))
        .sub_agent("researcher", &child)
        .build()
        .expect("stream observer parent");
    let config = vv_agent::RunConfig::builder()
        .max_cycles(2)
        .trace_id("trace-stream-panic")
        .stream(move |event| {
            if matches!(event.payload(), RunEventPayload::AssistantDelta { .. }) {
                observer_calls_for_callback.fetch_add(1, Ordering::SeqCst);
                panic!("caller stream observer panicked");
            }
        })
        .build();

    let result = runner
        .run_with_config(&agent, "Delegate", config)
        .await
        .expect("parent run survives configured child observer panic");
    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(observer_calls.load(Ordering::SeqCst), 1);
    assert!(result.events().iter().any(|event| {
        matches!(
            event.payload(),
            RunEventPayload::SubRunCompleted {
                status: AgentStatus::Completed,
                ..
            }
        )
    }));
}

#[test]
fn trusted_stream_event_failure_remains_fail_closed() {
    assert_eq!(
        contract()["stream_forwarding"]["event_sink_failure_policy"],
        "fail_closed"
    );
    let manager = SubTaskManager::default();
    let lifecycle = Arc::new(Mutex::new(Vec::<(String, BTreeMap<String, Value>)>::new()));
    let lifecycle_for_handler = lifecycle.clone();
    let event_handler: vv_agent::RunEventHandler = Arc::new(move |run_event| {
        let (name, payload) = typed_event_parts(run_event);
        if name == "assistant_delta" {
            panic!("run event store append failed: store down");
        }
        if matches!(name.as_str(), "sub_run_started" | "sub_run_completed") {
            lifecycle_for_handler
                .lock()
                .expect("fail-closed lifecycle")
                .push((name.to_string(), payload.clone()));
        }
    });

    let result = AgentRuntime::new(ObserverPanicConfiguredStreamClient::default())
        .run_with_controls(
            stream_observer_parent_task("fail-closed-parent"),
            RuntimeRunControls {
                event_handler: Some(event_handler),
                execution_context: Some(ExecutionContext {
                    metadata: BTreeMap::from([
                        ("_vv_agent_run_id".to_string(), json!("parent-run")),
                        (
                            "_vv_agent_trace_id".to_string(),
                            json!("trace-stream-fail-closed"),
                        ),
                    ]),
                    ..ExecutionContext::default()
                }),
                run_context: Some(RunContext {
                    run_id: "parent-run".to_string(),
                    agent_name: "parent".to_string(),
                    ..RunContext::default()
                }),
                sub_task_manager: Some(manager.clone()),
                ..RuntimeRunControls::default()
            },
        )
        .expect("parent receives failed configured child outcome");
    assert_eq!(result.status, AgentStatus::Completed);

    let task_id = delegated_task_id(&result);
    let child = manager.get(&task_id).expect("failed child snapshot");
    let outcome = child.outcome.expect("failed child outcome");
    assert_eq!(outcome.status, AgentStatus::Failed);
    assert_eq!(
        outcome.error.as_deref(),
        Some("run event store append failed: store down")
    );
    assert_eq!(outcome.error_code.as_deref(), Some("sub_task_failed"));

    let lifecycle = lifecycle.lock().expect("fail-closed lifecycle");
    assert_eq!(
        lifecycle
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        vec!["sub_run_started", "sub_run_completed"]
    );
    assert_eq!(lifecycle[1].1["status"], "failed");
}

#[test]
fn trusted_lifecycle_sink_panics_fail_closed_and_keep_one_event_pair() {
    for panic_on in ["sub_run_started", "sub_run_completed"] {
        let manager = SubTaskManager::default();
        let lifecycle = Arc::new(Mutex::new(Vec::<String>::new()));
        let lifecycle_for_handler = lifecycle.clone();
        let panicked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let panicked_for_handler = panicked.clone();
        let event_handler: vv_agent::RunEventHandler = Arc::new(move |run_event| {
            let (name, _payload) = typed_event_parts(run_event);
            if matches!(name.as_str(), "sub_run_started" | "sub_run_completed") {
                lifecycle_for_handler
                    .lock()
                    .expect("trusted lifecycle calls")
                    .push(name.to_string());
            }
            if name == panic_on && !panicked_for_handler.swap(true, Ordering::SeqCst) {
                panic!("trusted lifecycle sink failed on {panic_on}");
            }
        });

        let result = AgentRuntime::new(ObserverPanicConfiguredStreamClient::default())
            .run_with_controls(
                stream_observer_parent_task(&format!("lifecycle-sink-{panic_on}")),
                RuntimeRunControls {
                    event_handler: Some(event_handler),
                    sub_task_manager: Some(manager.clone()),
                    ..RuntimeRunControls::default()
                },
            )
            .expect("parent receives fail-closed child outcome");
        let task_id = delegated_task_id(&result);
        let child = manager
            .get(&task_id)
            .expect("lifecycle sink child snapshot");
        let outcome = child.outcome.expect("lifecycle sink child outcome");

        assert_eq!(result.status, AgentStatus::Completed, "{panic_on}");
        assert_eq!(outcome.status, AgentStatus::Failed, "{panic_on}");
        assert!(outcome
            .error
            .as_deref()
            .is_some_and(|error| error.contains("trusted lifecycle sink failed")));
        assert_eq!(outcome.error_code.as_deref(), Some("sub_task_failed"));
        assert!(!child.running);
        assert_eq!(
            lifecycle
                .lock()
                .expect("trusted lifecycle calls")
                .as_slice(),
            ["sub_run_started", "sub_run_completed"],
            "{panic_on}"
        );
    }
}
