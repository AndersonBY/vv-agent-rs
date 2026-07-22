use super::*;

#[test]
fn handler_injects_lineage_for_sync_async_and_batch_requests() {
    let registry = build_default_registry();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let captured_for_runner = captured.clone();
    let manager = SubTaskManager::default();
    let mut context = ToolContext::new(".");
    context.task_id = "parent-task".to_string();
    context.run_context = Some(RunContext {
        run_id: "parent-run".to_string(),
        agent_name: "parent".to_string(),
        ..RunContext::default()
    });
    context
        .metadata
        .insert("_vv_agent_run_id".to_string(), json!("execution-run"));
    context.sub_task_manager = Some(manager.clone());
    context.sub_task_runner = Some(Arc::new(move |request| {
        captured_for_runner
            .lock()
            .expect("captured requests")
            .push(request.clone());
        completed_outcome(request)
    }));

    for (tool_call_id, arguments) in [
        (
            "sync-call",
            BTreeMap::from([
                ("agent_id".to_string(), json!("researcher")),
                ("task_description".to_string(), json!("sync")),
            ]),
        ),
        (
            "batch-call",
            BTreeMap::from([
                ("agent_id".to_string(), json!("researcher")),
                (
                    "tasks".to_string(),
                    json!([{"task_description": "batch-a"}, {"task_description": "batch-b"}]),
                ),
            ]),
        ),
        (
            "async-call",
            BTreeMap::from([
                ("agent_id".to_string(), json!("researcher")),
                ("task_description".to_string(), json!("async")),
                ("wait_for_completion".to_string(), json!(false)),
            ]),
        ),
    ] {
        let result = registry
            .execute(
                &ToolCall::new(tool_call_id, "create_sub_task", arguments),
                &mut context,
            )
            .expect("create_sub_task");
        assert_eq!(result.status, ToolResultStatus::Success);
        if tool_call_id == "async-call" {
            let payload: Value = serde_json::from_str(&result.content).expect("async payload");
            let task_id = payload["task_id"].as_str().expect("async task id");
            assert!(manager.wait(task_id, Some(Duration::from_secs(2))));
            let snapshot = manager.get(task_id).expect("async snapshot");
            assert_eq!(snapshot.parent_run_id.as_deref(), Some("parent-run"));
            assert_eq!(snapshot.parent_tool_call_id.as_deref(), Some("async-call"));
        }
    }

    context.run_context = None;
    context.metadata.remove("_vv_agent_run_id");
    let result = registry
        .execute(
            &ToolCall::new(
                "missing-run-call",
                "create_sub_task",
                BTreeMap::from([
                    ("agent_id".to_string(), json!("researcher")),
                    ("task_description".to_string(), json!("missing lineage")),
                ]),
            ),
            &mut context,
        )
        .expect("create_sub_task without parent run identity");
    assert_eq!(result.status, ToolResultStatus::Success);

    let captured = captured.lock().expect("captured requests");
    assert_eq!(captured.len(), 5);
    let expected_calls = ["sync-call", "batch-call", "batch-call", "async-call"];
    for (request, expected_call) in captured.iter().zip(expected_calls) {
        assert_eq!(
            request.metadata.get("parent_run_id"),
            Some(&json!("parent-run"))
        );
        assert_eq!(
            request.metadata.get("parent_tool_call_id"),
            Some(&json!(expected_call))
        );
    }
    assert!(!captured[4].metadata.contains_key("parent_run_id"));
    assert_eq!(
        captured[4].metadata.get("parent_tool_call_id"),
        Some(&json!("missing-run-call"))
    );
}

#[test]
fn handler_ignores_fixture_non_string_lineage_and_falls_back_from_blank_public_id() {
    let fixture = contract();
    assert_eq!(
        fixture["identity"]["non_string_metadata_policy"],
        "ignore_and_fall_through"
    );
    let invalid_values = fixture["identity"]["non_string_metadata_values"]
        .as_array()
        .expect("non-string metadata values");
    let registry = build_default_registry();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let captured_for_runner = captured.clone();
    let mut context = ToolContext::new(".");
    context.task_id = "parent-task".to_string();
    context.sub_task_runner = Some(Arc::new(move |request| {
        captured_for_runner
            .lock()
            .expect("captured strict lineage requests")
            .push(request.clone());
        completed_outcome(request)
    }));

    for (index, invalid) in invalid_values.iter().enumerate() {
        context.run_context = Some(RunContext {
            run_id: " \n ".to_string(),
            ..RunContext::default()
        });
        context
            .metadata
            .insert("_vv_agent_run_id".to_string(), json!("execution-run"));
        let fallback_call_id = format!("fallback-{index}");
        let fallback_result = registry
            .execute(
                &ToolCall::new(
                    &fallback_call_id,
                    "create_sub_task",
                    BTreeMap::from([
                        ("agent_id".to_string(), json!("researcher")),
                        (
                            "task_description".to_string(),
                            json!("Use execution lineage"),
                        ),
                    ]),
                ),
                &mut context,
            )
            .expect("fallback lineage result");
        assert_eq!(fallback_result.status, ToolResultStatus::Success);

        context.run_context = None;
        context
            .metadata
            .insert("_vv_agent_run_id".to_string(), invalid.clone());
        let missing_call_id = format!("missing-{index}");
        let missing_result = registry
            .execute(
                &ToolCall::new(
                    &missing_call_id,
                    "create_sub_task",
                    BTreeMap::from([
                        ("agent_id".to_string(), json!("researcher")),
                        ("task_description".to_string(), json!("Use no run lineage")),
                    ]),
                ),
                &mut context,
            )
            .expect("missing lineage result");
        assert_eq!(missing_result.status, ToolResultStatus::Success);
    }

    let captured = captured.lock().expect("captured strict lineage requests");
    assert_eq!(captured.len(), invalid_values.len() * 2);
    for (index, requests) in captured.chunks_exact(2).enumerate() {
        assert_eq!(requests[0].metadata["parent_run_id"], "execution-run");
        assert_eq!(
            requests[0].metadata["parent_tool_call_id"],
            format!("fallback-{index}")
        );
        assert!(!requests[1].metadata.contains_key("parent_run_id"));
        assert_eq!(
            requests[1].metadata["parent_tool_call_id"],
            format!("missing-{index}")
        );
    }
}

fn run_real_identity_case(
    run_context: Option<RunContext>,
    execution_metadata: BTreeMap<String, Value>,
    task_trace_id: Option<Value>,
    request_parent_run_id: Option<Value>,
) -> (
    CapturedRuntimeEvents,
    vv_agent::runtime::ManagedSubTaskSnapshot,
    Value,
) {
    let mut registry = build_default_registry();
    registry
        .register(ToolSpec::new(
            "identity_delegate",
            "Run one configured child identity case.",
            Arc::new(move |context, _arguments| {
                let runner = context
                    .sub_task_runner
                    .clone()
                    .expect("configured identity runner");
                let mut request = SubTaskRequest::new("researcher", "Collect facts");
                request.metadata.insert(
                    "parent_tool_call_id".to_string(),
                    json!(context.tool_call_id),
                );
                if let Some(parent_run_id) = &request_parent_run_id {
                    request
                        .metadata
                        .insert("parent_run_id".to_string(), parent_run_id.clone());
                }
                let outcome = runner(request);
                ToolExecutionResult::success(
                    "",
                    serde_json::to_string(&outcome).expect("serialize identity outcome"),
                )
            }),
        ))
        .expect("register identity delegate");
    let inspected_child_identity = Arc::new(Mutex::new(None));
    let inspected_child_identity_for_tool = inspected_child_identity.clone();
    registry
        .register(ToolSpec::new(
            "inspect_child_identity",
            "Capture the real configured child identity.",
            Arc::new(move |context, _arguments| {
                let run_context = context
                    .run_context
                    .as_ref()
                    .expect("configured child run context");
                *inspected_child_identity_for_tool
                    .lock()
                    .expect("inspected child identity") = Some(json!({
                    "run_id": run_context.run_id,
                    "run_context_trace_id": run_context.metadata.get("trace_id"),
                    "execution_trace_id": context.metadata.get("_vv_agent_trace_id"),
                }));
                ToolExecutionResult::success("", "captured")
            }),
        ))
        .expect("register child identity inspection tool");
    let llm = ScriptedLlmClient::from_steps(vec![
        ScriptStep::response(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "delegate",
                "identity_delegate",
                json!({}),
            )],
        )),
        ScriptStep::response(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "inspect-child",
                "inspect_child_identity",
                json!({}),
            )],
        )),
        ScriptStep::response(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "child-finish",
                "task_finish",
                json!({"message": "child done"}),
            )],
        )),
        ScriptStep::response(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "parent-finish",
                "task_finish",
                json!({"message": "parent done"}),
            )],
        )),
    ]);
    let mut parent = AgentTask::new("parent-task", "shared-model", "Parent prompt", "Delegate");
    parent.max_cycles = 3;
    parent.extra_tool_names = vec![
        "identity_delegate".to_string(),
        "inspect_child_identity".to_string(),
    ];
    if let Some(trace_id) = task_trace_id {
        parent.metadata.insert("trace_id".to_string(), trace_id);
    }
    parent.sub_agents.insert(
        "researcher".to_string(),
        SubAgentConfig::new("shared-model", "Research"),
    );
    let manager = SubTaskManager::default();
    let lifecycle = Arc::new(Mutex::new(Vec::new()));
    let lifecycle_for_handler = lifecycle.clone();
    let event_handler: vv_agent::RunEventHandler = Arc::new(move |run_event| {
        let (name, payload) = typed_event_parts(run_event);
        if matches!(name.as_str(), "sub_run_started" | "sub_run_completed") {
            lifecycle_for_handler
                .lock()
                .expect("identity lifecycle")
                .push((name.to_string(), payload.clone()));
        }
    });

    let result = AgentRuntime::new(llm)
        .with_tool_registry(registry)
        .run_with_controls(
            parent,
            RuntimeRunControls {
                event_handler: Some(event_handler),
                execution_context: Some(ExecutionContext {
                    metadata: execution_metadata,
                    ..ExecutionContext::default()
                }),
                run_context,
                sub_task_manager: Some(manager.clone()),
                ..RuntimeRunControls::default()
            },
        )
        .expect("real configured identity case");
    assert_eq!(result.status, AgentStatus::Completed);
    let outcome: SubTaskOutcome = serde_json::from_str(&result.cycles[0].tool_results[0].content)
        .expect("identity child outcome");
    let snapshot = manager
        .get(&outcome.task_id)
        .expect("identity manager record");
    let lifecycle = lifecycle.lock().expect("identity lifecycle").clone();
    let inspected_child_identity = inspected_child_identity
        .lock()
        .expect("inspected child identity")
        .clone()
        .expect("real child inspection payload");
    (lifecycle, snapshot, inspected_child_identity)
}

#[test]
fn real_runtime_lineage_uses_three_level_precedence_without_task_id_fabrication() {
    let fixture = contract();
    let manager_contract = &fixture["manager"];
    assert_eq!(
        manager_contract["parent_lineage_precedence"],
        json!(["run_context", "execution_context", "request_metadata"])
    );
    let cases = [
        (
            Some(RunContext {
                run_id: "public-run".to_string(),
                ..RunContext::default()
            }),
            BTreeMap::from([("_vv_agent_run_id".to_string(), json!("execution-run"))]),
            Some(json!("request-run")),
            Some("public-run"),
        ),
        (
            None,
            BTreeMap::from([("_vv_agent_run_id".to_string(), json!("execution-run"))]),
            Some(json!("request-run")),
            Some("execution-run"),
        ),
        (
            None,
            BTreeMap::new(),
            Some(json!("request-run")),
            Some("request-run"),
        ),
        (None, BTreeMap::new(), None, None),
    ];

    for (run_context, execution_metadata, request_parent_run_id, expected) in cases {
        let (events, snapshot, _) =
            run_real_identity_case(run_context, execution_metadata, None, request_parent_run_id);
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].1.get("parent_run_id").and_then(Value::as_str),
            expected
        );
        assert_eq!(snapshot.parent_run_id.as_deref(), expected);
        assert_eq!(snapshot.parent_tool_call_id.as_deref(), Some("delegate"));
        if expected.is_none() {
            assert_eq!(
                snapshot.parent_run_id.as_deref() == Some(snapshot.task_id.as_str()),
                manager_contract["fabricates_parent_run_id_from_task_id"]
            );
        }
    }
}

#[derive(Clone)]
struct AsyncLineageClient {
    child_requests: Arc<Mutex<Vec<LlmRequest>>>,
    parent_calls: Arc<AtomicUsize>,
}

impl LlmClient for AsyncLineageClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let is_child = request.messages.first().is_some_and(|message| {
            message.role == vv_agent::MessageRole::System && message.content == "Child prompt"
        });
        if is_child {
            self.child_requests
                .lock()
                .expect("async lineage child requests")
                .push(request);
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "child-finish",
                    "task_finish",
                    json!({"message": "child done"}),
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
                        "task_description": "Inspect lineage",
                        "wait_for_completion": false
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

#[test]
fn real_async_initial_lineage_ignores_parent_task_runtime_identity_metadata() {
    let fixture = contract();
    let manager_contract = &fixture["manager"];
    let child_requests = Arc::new(Mutex::new(Vec::<LlmRequest>::new()));
    let llm = AsyncLineageClient {
        child_requests: child_requests.clone(),
        parent_calls: Arc::new(AtomicUsize::new(0)),
    };
    let manager = SubTaskManager::default();
    let lifecycle = Arc::new(Mutex::new(Vec::new()));
    let lifecycle_for_handler = lifecycle.clone();
    let event_handler: vv_agent::RunEventHandler = Arc::new(move |run_event| {
        let (name, payload) = typed_event_parts(run_event);
        if matches!(name.as_str(), "sub_run_started" | "sub_run_completed") {
            lifecycle_for_handler
                .lock()
                .expect("async initial lineage lifecycle")
                .push((name.to_string(), payload.clone()));
        }
    });
    let mut parent = AgentTask::new(
        "lineage-parent",
        "shared-model",
        "Parent prompt",
        "Delegate",
    );
    parent.max_cycles = 3;
    parent.metadata.extend(BTreeMap::from([
        ("_vv_agent_run_id".to_string(), json!("spoof-task-run")),
        ("_vv_agent_trace_id".to_string(), json!("spoof-task-trace")),
        (
            "_vv_agent_parent_run_id".to_string(),
            json!("spoof-grandparent"),
        ),
        (
            "_vv_agent_parent_tool_call_id".to_string(),
            json!("spoof-parent-tool"),
        ),
    ]));
    let mut child = SubAgentConfig::new("shared-model", "Research");
    child.system_prompt = Some("Child prompt".to_string());
    parent.sub_agents.insert("researcher".to_string(), child);

    let result = AgentRuntime::new(llm)
        .run_with_controls(
            parent,
            RuntimeRunControls {
                event_handler: Some(event_handler),
                execution_context: Some(ExecutionContext {
                    metadata: BTreeMap::from([
                        ("_vv_agent_run_id".to_string(), json!("execution-run")),
                        ("_vv_agent_trace_id".to_string(), json!("execution-trace")),
                    ]),
                    ..ExecutionContext::default()
                }),
                run_context: Some(RunContext {
                    run_id: "public-run".to_string(),
                    agent_name: "parent".to_string(),
                    ..RunContext::default()
                }),
                sub_task_manager: Some(manager.clone()),
                ..RuntimeRunControls::default()
            },
        )
        .expect("async configured child with spoofed task metadata");
    let payload: Value = serde_json::from_str(&result.cycles[0].tool_results[0].content)
        .expect("async lineage payload");
    let task_id = payload["task_id"].as_str().expect("async lineage task id");
    assert!(manager.wait(task_id, Some(Duration::from_secs(3))));
    let snapshot = manager.get(task_id).expect("async lineage snapshot");
    let events = lifecycle.lock().expect("async lineage lifecycle");
    let requests = child_requests.lock().expect("async lineage child requests");

    assert_eq!(
        manager_contract["initial_lineage_ignores_task_metadata"],
        true
    );
    assert_eq!(snapshot.parent_run_id.as_deref(), Some("public-run"));
    assert_eq!(snapshot.parent_tool_call_id.as_deref(), Some("delegate"));
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|(_, event)| {
        event["parent_run_id"] == "public-run"
            && event["parent_tool_call_id"] == "delegate"
            && event["trace_id"] == "execution-trace"
    }));
    assert!(!requests.is_empty());
    assert_eq!(requests[0].metadata["parent_run_id"], "public-run");
    assert_eq!(requests[0].metadata["parent_tool_call_id"], "delegate");
    assert_eq!(
        requests[0].metadata["_vv_agent_trace_id"],
        "execution-trace"
    );
    assert_ne!(requests[0].metadata["parent_run_id"], "spoof-task-run");
}

#[test]
fn real_runtime_trace_identity_follows_fixture_precedence_and_child_run_fallback() {
    let fixture = contract();
    let identity = &fixture["identity"];
    let sources = identity["trace_precedence"]
        .as_array()
        .expect("trace precedence");
    let cases = [
        (
            Some("execution-trace"),
            Some("public-trace"),
            Some("task-trace"),
            Some("execution-trace"),
        ),
        (
            None,
            Some("public-trace"),
            Some("task-trace"),
            Some("public-trace"),
        ),
        (None, None, Some("task-trace"), Some("task-trace")),
        (None, None, None, None),
    ];
    assert_eq!(sources.len(), cases.len());

    for (index, (execution_trace, public_trace, task_trace, expected_trace)) in
        cases.into_iter().enumerate()
    {
        let mut execution_metadata =
            BTreeMap::from([("_vv_agent_run_id".to_string(), json!("execution-run"))]);
        if let Some(trace_id) = execution_trace {
            execution_metadata.insert("_vv_agent_trace_id".to_string(), json!(trace_id));
        }
        let mut run_context = RunContext {
            run_id: "public-run".to_string(),
            ..RunContext::default()
        };
        if let Some(trace_id) = public_trace {
            run_context
                .metadata
                .insert("trace_id".to_string(), json!(trace_id));
        }
        let (events, _, inspected) = run_real_identity_case(
            Some(run_context),
            execution_metadata,
            task_trace.map(|value| json!(value)),
            Some(json!("request-run")),
        );
        let source = sources[index].as_str().expect("trace source");
        let expected_trace = if source
            == identity["trace_fallback"]
                .as_str()
                .expect("trace fallback source")
        {
            events[0].1["run_id"].as_str().expect("child run id")
        } else {
            expected_trace.expect("explicit trace source")
        };
        assert!(events
            .iter()
            .all(|(_, payload)| payload["trace_id"] == expected_trace));
        assert_eq!(inspected["run_context_trace_id"], expected_trace);
        assert_eq!(inspected["execution_trace_id"], expected_trace);
    }
}

#[test]
fn real_child_context_uses_canonical_trace_for_fixture_non_string_metadata() {
    let fixture = contract();
    let identity = &fixture["identity"];
    assert_eq!(
        identity["non_string_metadata_policy"],
        "ignore_and_fall_through"
    );

    for invalid in identity["non_string_metadata_values"]
        .as_array()
        .expect("non-string metadata values")
    {
        let run_context = RunContext {
            metadata: BTreeMap::from([("trace_id".to_string(), invalid.clone())]),
            ..RunContext::default()
        };
        let execution_metadata = BTreeMap::from([
            ("_vv_agent_run_id".to_string(), invalid.clone()),
            ("_vv_agent_trace_id".to_string(), invalid.clone()),
            ("trace_id".to_string(), invalid.clone()),
        ]);
        let (events, snapshot, inspected) = run_real_identity_case(
            Some(run_context),
            execution_metadata,
            Some(invalid.clone()),
            Some(invalid.clone()),
        );

        let canonical_trace_id = events[0].1["run_id"]
            .as_str()
            .expect("generated child run id");
        assert!(events
            .iter()
            .all(|(_, payload)| payload["trace_id"] == canonical_trace_id));
        assert_eq!(snapshot.parent_run_id, None);
        assert_eq!(inspected["run_id"], canonical_trace_id);
        assert_eq!(inspected["run_context_trace_id"], canonical_trace_id);
        assert_eq!(inspected["execution_trace_id"], canonical_trace_id);
    }
}
