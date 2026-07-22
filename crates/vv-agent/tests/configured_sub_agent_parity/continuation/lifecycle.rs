use super::*;

#[test]
fn cancelling_a_child_token_does_not_cancel_its_parent() {
    let fixture = contract();
    let parent = vv_agent::CancellationToken::default();
    let child = parent.child();

    child.cancel();

    assert!(child.is_cancelled());
    assert_eq!(
        !parent.is_cancelled(),
        fixture["cancellation"]["child_does_not_cancel_parent"]
    );
}

#[test]
fn agent_task_wire_preserves_effective_model_settings_messages_and_state() {
    let mut task = AgentTask::new("task", "model", "system", "input");
    task.model_settings = Some(vv_agent::ModelSettings {
        temperature: Some(0.25),
        max_tokens: Some(512),
        ..vv_agent::ModelSettings::default()
    });
    task.initial_messages = vec![vv_agent::Message::user("persisted")];
    task.initial_shared_state = BTreeMap::from([("scope".to_string(), json!("child"))]);

    let restored: AgentTask =
        serde_json::from_value(serde_json::to_value(&task).expect("serialize agent task"))
            .expect("restore agent task");
    assert_eq!(restored, task);
}

#[test]
fn sub_task_outcome_omits_an_absent_optional_error_code() {
    let current = json!({
        "task_id": "child-task",
        "agent_name": "researcher",
        "status": "failed",
        "session_id": "child-session",
        "final_answer": null,
        "wait_reason": null,
        "error": "failed",
        "cycles": 0,
        "todo_list": [],
        "resolved": {}
    });
    let restored: vv_agent::SubTaskOutcome =
        serde_json::from_value(current).expect("current outcome");
    assert!(restored.error_code.is_none());
    assert!(serde_json::to_value(restored)
        .expect("serialize outcome")
        .get("error_code")
        .is_none());
}

#[test]
fn empty_tool_call_id_policy_drops_only_the_incomplete_turn() {
    let fixture = contract();
    assert_eq!(
        fixture["continuation"]["empty_tool_call_id_policy"],
        "drop_incomplete_turn"
    );
    let messages = vec![
        vv_agent::Message {
            tool_calls: vec![
                ToolCall::new("", "read_file", BTreeMap::new()),
                ToolCall::new("complete-call", "read_file", BTreeMap::new()),
            ],
            ..vv_agent::Message::assistant("Working")
        },
        vv_agent::Message::tool("complete result", "complete-call"),
    ];

    let sanitized = vv_agent::sanitize_for_resume(&messages);

    assert_eq!(sanitized.len(), 2);
    assert_eq!(sanitized[0].tool_calls.len(), 1);
    assert_eq!(sanitized[0].tool_calls[0].id, "complete-call");
    assert_eq!(sanitized[1].tool_call_id.as_deref(), Some("complete-call"));
}

#[test]
fn real_continuation_preserves_complete_history_state_and_lineage() {
    let fixture = contract();
    let continuation_contract = &fixture["continuation"];
    let continuation_messages = Arc::new(Mutex::new(Vec::<Vec<vv_agent::Message>>::new()));
    let first_continuation_messages = continuation_messages.clone();
    let second_continuation_messages = continuation_messages.clone();
    let continued_state = Arc::new(Mutex::new(None));
    let continued_state_for_tool = continued_state.clone();
    let lifecycle_events = Arc::new(Mutex::new(Vec::new()));
    let lifecycle_events_for_handler = lifecycle_events.clone();
    let shared_llm = ScriptedLlmClient::from_steps(vec![
        ScriptStep::response(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "delegate",
                "create_sub_task",
                json!({"agent_id": "researcher", "task_description": "first prompt"}),
            )],
        )),
        ScriptStep::response(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "set-state",
                "child_state",
                json!({"mode": "set"}),
            )],
        )),
        ScriptStep::response(finish_response("first-finish", "first answer")),
        ScriptStep::response(finish_response("parent-finish", "parent done")),
        ScriptStep::callback(move |request| {
            first_continuation_messages
                .lock()
                .expect("continuation messages")
                .push(request.messages.clone());
            Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "inspect-state",
                    "child_state",
                    json!({"mode": "inspect"}),
                )],
            ))
        }),
        ScriptStep::response(finish_response("second-finish", "second answer")),
        ScriptStep::callback(move |request| {
            second_continuation_messages
                .lock()
                .expect("continuation messages")
                .push(request.messages.clone());
            Ok(finish_response("third-finish", "third answer"))
        }),
    ]);
    let mut registry = build_default_registry();
    registry
        .register_tool_with_parameters(
            "child_state",
            "Set or inspect child shared state.",
            json!({
                "type": "object",
                "properties": {
                    "mode": {
                        "type": "string",
                        "enum": ["set", "inspect"]
                    }
                },
                "required": ["mode"]
            }),
            Arc::new(move |context, arguments| {
                match arguments.get("mode").and_then(Value::as_str) {
                    Some("set") => {
                        context
                            .shared_state
                            .insert("child_value".to_string(), json!("preserved"));
                    }
                    Some("inspect") => {
                        *continued_state_for_tool.lock().expect("continued state") =
                            context.shared_state.get("child_value").cloned();
                    }
                    _ => {}
                }
                ToolExecutionResult::success("", json!({"ok": true}).to_string())
            }),
        )
        .expect("register child state tool");
    let manager = SubTaskManager::default();
    let runtime = AgentRuntime::new(shared_llm).with_tool_registry(registry);
    let mut parent = AgentTask::new("parent-task", "shared-model", "Parent prompt", "Delegate");
    parent.max_cycles = 4;
    parent.extra_tool_names = vec!["child_state".to_string()];
    let mut child = SubAgentConfig::new("shared-model", "Research");
    child.max_cycles = 4;
    parent.sub_agents.insert("researcher".to_string(), child);
    let event_handler: vv_agent::RunEventHandler = Arc::new(move |run_event| {
        let (name, payload) = typed_event_parts(run_event);
        if matches!(name.as_str(), "sub_run_started" | "sub_run_completed") {
            lifecycle_events_for_handler
                .lock()
                .expect("lifecycle events")
                .push((name.to_string(), payload.clone()));
        }
    });
    let controls = RuntimeRunControls {
        event_handler: Some(event_handler),
        execution_context: Some(ExecutionContext {
            metadata: BTreeMap::from([
                ("_vv_agent_run_id".to_string(), json!("parent-run")),
                ("_vv_agent_trace_id".to_string(), json!("trace-parity")),
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
    };

    let result = runtime
        .run_with_controls(parent, controls)
        .expect("initial parent run");
    let initial_payload: Value = serde_json::from_str(&result.cycles[0].tool_results[0].content)
        .expect("initial child payload");
    let task_id = initial_payload["task_id"]
        .as_str()
        .expect("child task id")
        .to_string();
    manager
        .continue_task(&task_id, "second prompt")
        .expect("continue child task");
    assert!(manager.wait(&task_id, Some(Duration::from_secs(2))));
    let snapshot = manager.get(&task_id).expect("continued snapshot");

    assert_eq!(
        snapshot
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.final_answer.as_deref()),
        Some("second answer")
    );
    assert_eq!(
        continued_state.lock().expect("continued state").as_ref(),
        Some(&json!("preserved"))
    );
    manager
        .continue_task(&task_id, "third prompt")
        .expect("continue child task again");
    assert!(manager.wait(&task_id, Some(Duration::from_secs(2))));
    let final_snapshot = manager.get(&task_id).expect("second continued snapshot");
    assert_eq!(
        final_snapshot
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.final_answer.as_deref()),
        Some("third answer")
    );

    let continuation_messages = continuation_messages.lock().expect("continuation messages");
    assert_eq!(continuation_messages.len(), 2);
    let messages = &continuation_messages[0];
    let contents = messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>();
    let mut positions = Vec::new();
    for required in continuation_contract["required_history"]
        .as_array()
        .expect("required continuation history")
        .iter()
        .filter_map(Value::as_str)
    {
        positions.push(
            contents
                .iter()
                .position(|message| message.contains(required))
                .expect("required continuation message"),
        );
    }
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(contents
        .iter()
        .any(|message| message.contains("first-finish") || message.contains("first answer")));
    let assistant_tool_call_ids = messages
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .flat_map(|message| message.tool_calls.iter().map(|call| call.id.as_str()))
        .collect::<BTreeSet<_>>();
    let tool_result_ids = messages
        .iter()
        .filter(|message| message.role == MessageRole::Tool)
        .filter_map(|message| message.tool_call_id.as_deref())
        .collect::<BTreeSet<_>>();
    let complete_tool_turn =
        !assistant_tool_call_ids.is_empty() && assistant_tool_call_ids.is_subset(&tool_result_ids);
    assert_eq!(
        complete_tool_turn,
        continuation_contract["include_complete_tool_turn"]
    );
    let state_preserved =
        continued_state.lock().expect("continued state").as_ref() == Some(&json!("preserved"));
    assert_eq!(
        state_preserved,
        continuation_contract["preserve_shared_state"]
    );
    let lifecycle = lifecycle_events.lock().expect("lifecycle events");
    assert_eq!(
        lifecycle
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "sub_run_started",
            "sub_run_completed",
            "sub_run_started",
            "sub_run_completed",
            "sub_run_started",
            "sub_run_completed"
        ]
    );
    assert_ne!(lifecycle[0].1["run_id"], lifecycle[2].1["run_id"]);
    assert_ne!(lifecycle[2].1["run_id"], lifecycle[4].1["run_id"]);
    assert_eq!(
        continuation_messages[0][0].metadata["_vv_agent_run_id"],
        lifecycle[2].1["run_id"]
    );
    assert_eq!(
        continuation_messages[1][0].metadata["_vv_agent_run_id"],
        lifecycle[4].1["run_id"]
    );
    assert_ne!(
        continuation_messages[0][0].metadata["_vv_agent_run_id"],
        lifecycle[0].1["run_id"]
    );
    for (_, payload) in lifecycle.iter() {
        assert_eq!(payload["parent_run_id"], "parent-run");
        assert_eq!(payload["parent_tool_call_id"], "delegate");
        assert_eq!(payload["child_session_id"], initial_payload["session_id"]);
    }
}
