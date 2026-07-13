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

#[test]
fn configured_child_stream_is_allowlisted_and_keeps_canonical_typed_identity() {
    let stream_contract = &contract()["stream_forwarding"];
    let typed_events = Arc::new(Mutex::new(Vec::new()));
    let event_context = RuntimeEventContext::new(
        "parent-stream-run",
        "trace-stream-contract",
        "parent",
        Some("parent-stream-session".to_string()),
        "Delegate",
    );
    let raw_streams = Arc::new(Mutex::new(Vec::<BTreeMap<String, Value>>::new()));
    let raw_streams_for_callback = raw_streams.clone();
    let typed_events_for_callback = typed_events.clone();
    let callback_event_context = event_context.clone();
    let raw_stream_callback: LlmStreamCallback = Arc::new(move |payload| {
        raw_streams_for_callback
            .lock()
            .expect("configured raw streams")
            .push(payload.clone());
        if let Some(event) = callback_event_context.map_stream_payload(payload) {
            typed_events_for_callback
                .lock()
                .expect("configured typed callback events")
                .push(event);
        }
    });
    let typed_events_for_handler = typed_events.clone();
    let event_handler: vv_agent::RuntimeEventHandler = Arc::new(move |name, payload| {
        if let Some(event) = map_runtime_event(name, payload, &event_context) {
            typed_events_for_handler
                .lock()
                .expect("configured typed events")
                .push(event);
        }
    });
    let mut parent = AgentTask::new(
        "parent-stream-task",
        "shared-model",
        "Parent prompt",
        "Delegate",
    );
    parent.max_cycles = 2;
    let mut child = SubAgentConfig::new("shared-model", "Research");
    child.system_prompt = Some("Child prompt".to_string());
    parent.sub_agents.insert("researcher".to_string(), child);

    let result = AgentRuntime::new(MaliciousConfiguredStreamClient::default())
        .run_with_controls(
            parent,
            RuntimeRunControls {
                log_handler: Some(event_handler),
                execution_context: Some(ExecutionContext {
                    stream_callback: Some(raw_stream_callback),
                    metadata: BTreeMap::from([(
                        "_vv_agent_trace_id".to_string(),
                        json!("trace-stream-contract"),
                    )]),
                    ..ExecutionContext::default()
                }),
                run_context: Some(RunContext {
                    run_id: "parent-stream-run".to_string(),
                    agent_name: "parent".to_string(),
                    ..RunContext::default()
                }),
                ..RuntimeRunControls::default()
            },
        )
        .expect("configured malicious stream run");
    assert_eq!(result.status, AgentStatus::Completed);

    let raw_streams = raw_streams.lock().expect("configured raw streams");
    assert_eq!(raw_streams.len(), 5);
    let forged_parent_stream = raw_streams
        .iter()
        .find(|payload| payload["content_delta"] == "forged canonical parent delta")
        .expect("forged canonical parent stream");
    assert_eq!(forged_parent_stream["run_id"], "forged-run");
    let child_streams = raw_streams
        .iter()
        .filter(|payload| payload.get("parent_tool_call_id") == Some(&json!("delegate")))
        .collect::<Vec<_>>();
    assert_eq!(child_streams.len(), 4);
    assert_eq!(
        child_streams
            .iter()
            .map(|payload| payload["event"].clone())
            .collect::<Vec<_>>(),
        stream_contract["allowed_events"]
            .as_array()
            .expect("allowed configured stream events")
            .clone()
    );
    let identity_fields = stream_contract["canonical_identity_fields"]
        .as_array()
        .expect("canonical identity fields")
        .iter()
        .filter_map(Value::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    let reserved_fields = stream_contract["reserved_producer_fields"]
        .as_array()
        .expect("reserved producer fields")
        .iter()
        .filter_map(Value::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    let canonical_child_run_id = child_streams[0]["run_id"].clone();
    let canonical_child_session_id = child_streams[0]["session_id"].clone();
    let canonical_child_task_id = child_streams[0]["task_id"].clone();
    let expected_identity = BTreeMap::from([
        ("agent_name", json!("researcher")),
        ("child_run_id", canonical_child_run_id.clone()),
        ("child_session_id", canonical_child_session_id.clone()),
        ("parent_run_id", json!("parent-stream-run")),
        ("parent_tool_call_id", json!("delegate")),
        ("run_id", canonical_child_run_id),
        ("session_id", canonical_child_session_id),
        ("sub_agent_name", json!("researcher")),
        ("task_id", canonical_child_task_id),
        ("trace_id", json!("trace-stream-contract")),
    ]);
    for payload in &child_streams {
        let event = payload["event"].as_str().expect("stream event name");
        let producer_fields = stream_contract["producer_fields"][event]
            .as_array()
            .expect("producer fields")
            .iter()
            .filter_map(Value::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        let allowed_fields = producer_fields
            .union(&identity_fields)
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        let actual_fields = payload
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(actual_fields.is_subset(&allowed_fields));
        assert!(identity_fields.is_subset(&actual_fields));
        assert!(actual_fields
            .difference(&identity_fields)
            .all(|field| !reserved_fields.contains(field)));
        for (field, expected) in &expected_identity {
            assert_eq!(
                &payload[*field], expected,
                "canonical identity field {field}"
            );
        }
        assert!(!payload.contains_key("_vv_agent_stream_receipt"));
        assert!(!payload.contains_key("_vv_agent_stream_sequence"));
    }

    let typed_events = typed_events.lock().expect("configured typed events");
    let started = typed_events
        .iter()
        .find(|event| matches!(event.payload(), RunEventPayload::SubRunStarted { .. }))
        .expect("configured child started");
    let raw_delta = child_streams[0];
    assert_eq!(raw_delta["run_id"], started.run_id());
    assert_eq!(raw_delta["child_run_id"], started.run_id());
    assert_eq!(
        raw_delta["session_id"],
        started.session_id().expect("child session")
    );
    assert_eq!(
        raw_delta["child_session_id"],
        started.session_id().expect("child session")
    );
    assert_eq!(raw_delta["agent_name"], "researcher");
    assert_eq!(raw_delta["sub_agent_name"], "researcher");
    assert_eq!(raw_delta["trace_id"], "trace-stream-contract");
    assert_eq!(raw_delta["parent_run_id"], "parent-stream-run");
    assert_eq!(raw_delta["parent_tool_call_id"], "delegate");
    assert_eq!(raw_delta["content_delta"], "child delta");
    assert_eq!(raw_delta["estimated_tokens"], 2);
    assert!(!raw_delta.contains_key("cycle"));
    assert!(!raw_delta.contains_key("type"));
    assert!(!raw_delta.contains_key("status"));
    assert!(!raw_delta.contains_key("metadata"));
    assert!(!raw_delta.contains_key("unknown"));

    let typed_child_streams = typed_events
        .iter()
        .filter(|event| {
            event.run_id() == started.run_id()
                && event
                    .metadata
                    .get("event")
                    .and_then(Value::as_str)
                    .is_some()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        typed_child_streams
            .iter()
            .filter_map(|event| event.metadata.get("event"))
            .cloned()
            .collect::<Vec<_>>(),
        stream_contract["allowed_events"]
            .as_array()
            .expect("allowed configured stream events")
            .clone()
    );
    assert_eq!(typed_child_streams.len(), child_streams.len());
    for event in &typed_child_streams {
        assert_eq!(event.run_id(), started.run_id());
        assert_eq!(event.trace_id(), "trace-stream-contract");
        assert_eq!(event.agent_name(), Some("researcher"));
        assert_eq!(event.session_id(), started.session_id());
        assert_eq!(event.parent_run_id(), Some("parent-stream-run"));
        let source = event.metadata["event"]
            .as_str()
            .expect("typed child stream source");
        let raw = child_streams
            .iter()
            .find(|payload| payload["event"] == source)
            .expect("matching raw child stream");
        assert_eq!(
            serde_json::to_value(event).expect("typed child stream")["metadata"],
            serde_json::to_value(raw).expect("raw child stream")
        );
    }
    let typed_delta = typed_child_streams
        .iter()
        .find(|event| event.metadata["event"] == "assistant_delta")
        .expect("typed assistant delta");
    assert!(matches!(
        typed_delta.payload(),
        RunEventPayload::AssistantDelta { delta } if delta == "child delta"
    ));
    assert_eq!(typed_delta.run_id(), started.run_id());
    assert_eq!(typed_delta.trace_id(), "trace-stream-contract");
    assert_eq!(typed_delta.agent_name(), Some("researcher"));
    assert_eq!(typed_delta.session_id(), started.session_id());
    assert_eq!(typed_delta.parent_run_id(), Some("parent-stream-run"));
    assert_eq!(
        serde_json::to_value(typed_delta).expect("typed child delta")["metadata"],
        serde_json::to_value(raw_delta).expect("raw child delta")
    );
    let typed_reasoning = typed_child_streams
        .iter()
        .find(|event| event.metadata["event"] == "reasoning_delta")
        .expect("typed reasoning delta");
    assert!(matches!(
        typed_reasoning.payload(),
        RunEventPayload::AssistantDelta { delta } if delta == "child thought"
    ));
    for source in ["tool_call_started", "tool_call_progress"] {
        let typed_tool = typed_child_streams
            .iter()
            .find(|event| event.metadata["event"] == source)
            .expect("typed tool stream");
        assert!(matches!(
            typed_tool.payload(),
            RunEventPayload::ToolCallStarted {
                tool_call_id,
                tool_name,
                ..
            } if tool_call_id == "child-tool" && tool_name == "read_file"
        ));
    }
    let forged_typed = typed_events
        .iter()
        .find(|event| {
            matches!(
                event.payload(),
                RunEventPayload::AssistantDelta { delta }
                    if delta == "forged canonical parent delta"
            )
        })
        .expect("forged canonical shape remains a normal parent stream event");
    assert_eq!(forged_typed.run_id(), "parent-stream-run");
    assert_eq!(forged_typed.trace_id(), "trace-stream-contract");
    assert_eq!(forged_typed.agent_name(), Some("parent"));
    assert_eq!(forged_typed.session_id(), Some("parent-stream-session"));
    assert!(typed_events.iter().all(|event| {
        !(event.run_id() == started.run_id()
            && matches!(event.payload(), RunEventPayload::RunCompleted { .. }))
    }));
    assert_eq!(
        typed_events
            .iter()
            .filter(|event| {
                event.run_id() == "parent-stream-run"
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

#[test]
fn caller_stream_observer_panic_does_not_fail_or_orphan_configured_child() {
    assert_eq!(
        contract()["stream_forwarding"]["stream_observer_failure_isolated"],
        true
    );
    let manager = SubTaskManager::default();
    let lifecycle = Arc::new(Mutex::new(Vec::<(String, BTreeMap<String, Value>)>::new()));
    let lifecycle_for_handler = lifecycle.clone();
    let event_handler: vv_agent::RuntimeEventHandler = Arc::new(move |name, payload| {
        if matches!(name, "sub_run_started" | "sub_run_completed") {
            lifecycle_for_handler
                .lock()
                .expect("observer panic lifecycle")
                .push((name.to_string(), payload.clone()));
        }
    });
    let observer_calls = Arc::new(AtomicUsize::new(0));
    let observer_calls_for_callback = observer_calls.clone();
    let stream_callback: LlmStreamCallback = Arc::new(move |_| {
        observer_calls_for_callback.fetch_add(1, Ordering::SeqCst);
        panic!("caller stream observer panicked");
    });

    let result = AgentRuntime::new(ObserverPanicConfiguredStreamClient::default())
        .run_with_controls(
            stream_observer_parent_task("observer-panic-parent"),
            RuntimeRunControls {
                log_handler: Some(event_handler),
                execution_context: Some(ExecutionContext {
                    stream_callback: Some(stream_callback),
                    metadata: BTreeMap::from([
                        ("_vv_agent_run_id".to_string(), json!("parent-run")),
                        (
                            "_vv_agent_trace_id".to_string(),
                            json!("trace-stream-panic"),
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
        .expect("parent run survives configured child observer panic");
    assert_eq!(result.status, AgentStatus::Completed);

    let task_id = delegated_task_id(&result);
    let initial = manager.get(&task_id).expect("initial child snapshot");
    let initial_outcome = initial.outcome.expect("initial child outcome");
    assert_eq!(initial_outcome.status, AgentStatus::Completed);
    assert_eq!(
        initial_outcome.final_answer.as_deref(),
        Some("child answer 1")
    );
    assert!(initial_outcome.error.is_none());

    manager
        .continue_task(&task_id, "continue after observer panic")
        .expect("continue child after observer panic");
    assert!(manager.wait(&task_id, Some(Duration::from_secs(3))));
    let continued = manager.get(&task_id).expect("continued child snapshot");
    let continued_outcome = continued.outcome.expect("continued child outcome");
    assert_eq!(continued_outcome.status, AgentStatus::Completed);
    assert_eq!(
        continued_outcome.final_answer.as_deref(),
        Some("child answer 2")
    );
    assert!(continued_outcome.error.is_none());
    assert_eq!(observer_calls.load(Ordering::SeqCst), 2);

    let lifecycle = lifecycle.lock().expect("observer panic lifecycle");
    assert_eq!(
        lifecycle
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "sub_run_started",
            "sub_run_completed",
            "sub_run_started",
            "sub_run_completed"
        ]
    );
    assert!(lifecycle
        .iter()
        .filter(|(name, _)| name == "sub_run_completed")
        .all(|(_, payload)| payload["status"] == "completed"));
    assert_ne!(
        lifecycle[0].1["child_run_id"],
        lifecycle[2].1["child_run_id"]
    );
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
    let event_handler: vv_agent::RuntimeEventHandler = Arc::new(move |name, payload| {
        if name == "sub_agent_assistant_delta" {
            panic!("run event store append failed: store down");
        }
        if matches!(name, "sub_run_started" | "sub_run_completed") {
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
                log_handler: Some(event_handler),
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
        let event_handler: vv_agent::RuntimeEventHandler = Arc::new(move |name, _payload| {
            if matches!(name, "sub_run_started" | "sub_run_completed") {
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
                    log_handler: Some(event_handler),
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
