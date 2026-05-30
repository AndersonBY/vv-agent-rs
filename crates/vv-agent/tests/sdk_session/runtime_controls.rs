use super::*;

#[test]
fn session_runtime_receives_previous_messages_and_shared_state() {
    let responses = vec![
        LLMResponse {
            content: "record todo".to_string(),
            tool_calls: vec![ToolCall::new(
                "todo-1",
                "todo_write",
                json_args(serde_json::json!({
                    "todos": [
                        {"title": "carry context", "status": "completed", "priority": "medium"}
                    ]
                })),
            )],
            raw: BTreeMap::new(),
            token_usage: TokenUsage::default(),
        },
        LLMResponse {
            content: "finish first".to_string(),
            tool_calls: vec![ToolCall::new(
                "finish-1",
                "task_finish",
                json_args(serde_json::json!({"message": "first"})),
            )],
            raw: BTreeMap::new(),
            token_usage: TokenUsage::default(),
        },
        LLMResponse {
            content: "finish second".to_string(),
            tool_calls: vec![ToolCall::new(
                "finish-2",
                "task_finish",
                json_args(serde_json::json!({"message": "second"})),
            )],
            raw: BTreeMap::new(),
            token_usage: TokenUsage::default(),
        },
    ];
    let snapshots = Arc::new(Mutex::new(Vec::<RuntimeSnapshot>::new()));
    let mut runtime = AgentRuntime::new(ScriptedLlmClient::new(responses));
    runtime.hooks.push(Arc::new(RecordingRuntimeHook {
        snapshots: Arc::clone(&snapshots),
    }));
    let client = AgentSDKClient::new(AgentSDKOptions::default()).with_runtime(runtime);
    let mut session =
        create_agent_session(&client, "demo", AgentDefinition::default_for_model("demo"));

    let first = session.prompt("first").expect("first");
    let second = session.prompt("second").expect("second");

    assert_eq!(first.result.final_answer.as_deref(), Some("first"));
    assert_eq!(second.result.final_answer.as_deref(), Some("second"));
    let snapshots = snapshots.lock().expect("snapshots");
    let second_run_start = snapshots.last().expect("second run snapshot");
    assert!(
        second_run_start.messages.len() > 2,
        "second run should include previous session messages"
    );
    assert_eq!(second_run_start.messages.last().unwrap(), "user:second");
    assert_eq!(
        second_run_start.shared_state["todo_list"][0]["title"],
        Value::String("carry context".to_string())
    );
}

#[test]
fn session_runtime_injects_session_id_into_task_metadata() {
    let captured_metadata = Arc::new(Mutex::new(Vec::new()));
    let responses = vec![LLMResponse {
        content: "finish".to_string(),
        tool_calls: vec![ToolCall::new(
            "finish-1",
            "task_finish",
            json_args(serde_json::json!({"message": "ok"})),
        )],
        raw: BTreeMap::new(),
        token_usage: TokenUsage::default(),
    }];
    let mut runtime = AgentRuntime::new(ScriptedLlmClient::new(responses));
    runtime.hooks.push(Arc::new(TaskMetadataCaptureHook {
        captured_metadata: Arc::clone(&captured_metadata),
    }));
    let client = AgentSDKClient::new(AgentSDKOptions::default()).with_runtime(runtime);
    let mut session =
        create_agent_session(&client, "demo", AgentDefinition::default_for_model("demo"));

    let run = session.prompt("start").expect("prompt");

    assert_eq!(run.result.status, AgentStatus::Completed);
    let captured = captured_metadata.lock().expect("captured metadata");
    assert_eq!(captured.len(), 1);
    let session_id = captured[0]["session_id"]
        .as_str()
        .expect("session_id metadata");
    assert_eq!(session_id, session.session_id());
    assert_eq!(session_id.len(), 12);
    assert!(session_id.chars().all(|ch| ch.is_ascii_hexdigit()));
}

#[test]
fn session_runtime_uses_explicit_session_id_in_task_metadata() {
    let captured_metadata = Arc::new(Mutex::new(Vec::new()));
    let responses = vec![LLMResponse {
        content: "finish".to_string(),
        tool_calls: vec![ToolCall::new(
            "finish-1",
            "task_finish",
            json_args(serde_json::json!({"message": "ok"})),
        )],
        raw: BTreeMap::new(),
        token_usage: TokenUsage::default(),
    }];
    let mut runtime = AgentRuntime::new(ScriptedLlmClient::new(responses));
    runtime.hooks.push(Arc::new(TaskMetadataCaptureHook {
        captured_metadata: Arc::clone(&captured_metadata),
    }));
    let client = AgentSDKClient::new(AgentSDKOptions::default()).with_runtime(runtime);
    let mut session = client.create_session_with_id(
        "demo",
        AgentDefinition::default_for_model("demo"),
        "session-metadata-test",
    );

    let run = session.prompt("start").expect("prompt");

    assert_eq!(run.result.status, AgentStatus::Completed);
    assert_eq!(
        captured_metadata.lock().expect("captured metadata")[0]["session_id"],
        Value::String("session-metadata-test".to_string())
    );
    assert_eq!(session.state().session_id, "session-metadata-test");
}

#[test]
fn session_runtime_event_listener_can_queue_steering() {
    let responses = vec![
        LLMResponse {
            content: "two tool calls".to_string(),
            tool_calls: vec![
                ToolCall::new(
                    "todo-1",
                    "todo_write",
                    json_args(serde_json::json!({
                        "todos": [
                            {"title": "switch strategy", "status": "completed", "priority": "medium"}
                        ]
                    })),
                ),
                ToolCall::new(
                    "finish-should-skip",
                    "task_finish",
                    json_args(serde_json::json!({"message": "should be skipped"})),
                ),
            ],
            raw: BTreeMap::new(),
            token_usage: TokenUsage::default(),
        },
        LLMResponse {
            content: "finish after steering".to_string(),
            tool_calls: vec![ToolCall::new(
                "finish-2",
                "task_finish",
                json_args(serde_json::json!({"message": "done"})),
            )],
            raw: BTreeMap::new(),
            token_usage: TokenUsage::default(),
        },
    ];
    let client = AgentSDKClient::new(AgentSDKOptions::default())
        .with_runtime(AgentRuntime::new(ScriptedLlmClient::new(responses)));
    let mut session =
        create_agent_session(&client, "demo", AgentDefinition::default_for_model("demo"));
    let steering = session.steering_handle();
    let events = recorded_events();
    session.subscribe(recording_listener(&events));
    session.subscribe(Arc::new(move |event, payload| {
        if event == "tool_result"
            && payload.get("tool_name").and_then(Value::as_str) == Some("todo_write")
        {
            steering
                .steer("switch strategy before finishing")
                .expect("steer");
        }
    }));

    let run = session.prompt("start").expect("prompt");

    assert_eq!(run.result.status, AgentStatus::Completed);
    assert_eq!(run.result.final_answer.as_deref(), Some("done"));
    assert_eq!(
        run.result.cycles[0].tool_results[1].error_code.as_deref(),
        Some("skipped_due_to_steering")
    );
    let events = events.lock().expect("events");
    assert!(events.iter().any(|(event, _)| event == "tool_result"));
    assert!(events
        .iter()
        .any(|(event, _)| event == "session_steer_interrupt"));
    assert!(events.iter().any(|(event, _)| event == "run_steered"));
}

#[test]
fn sdk_options_log_preview_chars_configure_runtime_event_previews() {
    let assistant_text = "assistant sdk preview text ".repeat(4);
    let final_text = "final sdk preview text ".repeat(4);
    let responses = vec![LLMResponse {
        content: assistant_text.clone(),
        tool_calls: vec![ToolCall::new(
            "preview-finish",
            "task_finish",
            json_args(serde_json::json!({"message": final_text})),
        )],
        raw: BTreeMap::new(),
        token_usage: TokenUsage::default(),
    }];
    let client = AgentSDKClient::new(AgentSDKOptions {
        log_preview_chars: Some(10),
        ..AgentSDKOptions::default()
    })
    .with_runtime(AgentRuntime::new(ScriptedLlmClient::new(responses)));
    let mut session =
        create_agent_session(&client, "demo", AgentDefinition::default_for_model("demo"));
    let events = recorded_events();
    session.subscribe(recording_listener(&events));

    let run = session.prompt("start").expect("prompt");

    assert_eq!(run.result.status, AgentStatus::Completed);
    let events = events.lock().expect("events");
    let cycle_event = events
        .iter()
        .find(|(event, _)| event == "cycle_llm_response")
        .expect("cycle llm response");
    let completed_event = events
        .iter()
        .find(|(event, _)| event == "run_completed")
        .expect("run completed");
    assert_eq!(
        cycle_event.1["assistant_preview"],
        preview_text_for_test(&assistant_text, Some(10))
    );
    assert_eq!(
        completed_event.1["final_answer"],
        preview_text_for_test(run.result.final_answer.as_deref().expect("final"), Some(10))
    );
}

#[test]
fn session_auto_steers_when_background_command_finishes_during_run() {
    let responses = vec![
        LLMResponse {
            content: "start background command".to_string(),
            tool_calls: vec![
                ToolCall::new(
                    "bg-1",
                    "bash",
                    json_args(serde_json::json!({
                        "command": "printf bgdone",
                        "run_in_background": true,
                        "timeout": 5
                    })),
                ),
                ToolCall::new(
                    "slow-1",
                    "bash",
                    json_args(serde_json::json!({
                        "command": "sleep 1",
                        "timeout": 2
                    })),
                ),
                ToolCall::new(
                    "finish-should-skip",
                    "task_finish",
                    json_args(serde_json::json!({"message": "too early"})),
                ),
            ],
            raw: BTreeMap::new(),
            token_usage: TokenUsage::default(),
        },
        LLMResponse {
            content: "finish after background".to_string(),
            tool_calls: vec![ToolCall::new(
                "finish-2",
                "task_finish",
                json_args(serde_json::json!({"message": "noticed background"})),
            )],
            raw: BTreeMap::new(),
            token_usage: TokenUsage::default(),
        },
    ];
    let workspace = tempfile::tempdir().expect("workspace");
    let client = AgentSDKClient::new(AgentSDKOptions {
        workspace: workspace.path().to_path_buf(),
        ..AgentSDKOptions::default()
    })
    .with_runtime(AgentRuntime::new(ScriptedLlmClient::new(responses)));
    let mut session =
        create_agent_session(&client, "demo", AgentDefinition::default_for_model("demo"));
    let events = recorded_events();
    session.subscribe(recording_listener(&events));

    let run = session.prompt("start").expect("prompt");

    assert_eq!(run.result.status, AgentStatus::Completed);
    assert_eq!(
        run.result.final_answer.as_deref(),
        Some("noticed background")
    );
    assert_eq!(
        run.result.cycles[0].tool_results[2].error_code.as_deref(),
        Some("skipped_due_to_steering")
    );
    let events = events.lock().expect("events");
    assert!(events
        .iter()
        .any(|(event, _)| event == "background_command_completed"));
    assert!(events
        .iter()
        .any(|(event, _)| event == "background_command_terminal"));
    assert!(events.iter().any(|(event, _)| event == "run_steered"));
}
