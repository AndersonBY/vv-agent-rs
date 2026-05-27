use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use vv_agent::{
    create_agent_session, AgentDefinition, AgentRun, AgentRuntime, AgentSDKClient, AgentSDKOptions,
    AgentSession, AgentStatus, BeforeLlmEvent, BeforeToolCallEvent, BeforeToolCallPatch,
    CycleRecord, LLMResponse, ResolvedModelConfig, RuntimeHook, ScriptedLlmClient,
    SessionEventHandler, TokenUsage, ToolCall, ToolDirective, ToolExecutionResult,
};

fn preview_text_for_test(text: &str, log_preview_chars: Option<usize>) -> String {
    let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let Some(limit) = log_preview_chars.map(|limit| limit.max(40)) else {
        return cleaned;
    };
    if cleaned.chars().count() <= limit {
        return cleaned;
    }
    format!(
        "{}...",
        cleaned
            .chars()
            .take(limit.saturating_sub(3))
            .collect::<String>()
    )
}

#[test]
fn session_prompt_supports_follow_up_queue() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let execute_run = {
        let calls = Arc::clone(&calls);
        Arc::new(move |prompt: String| {
            calls.lock().expect("calls").push(prompt.clone());
            Ok(fake_run(&prompt, AgentStatus::Completed))
        })
    };
    let mut session = AgentSession::new(
        execute_run,
        "demo",
        AgentDefinition::default_for_model("demo"),
        "./workspace",
    );

    session.follow_up("after first run").expect("follow_up");
    let run = session.prompt("first run").expect("prompt");

    assert_eq!(run.result.status, AgentStatus::Completed);
    assert_eq!(
        *calls.lock().expect("calls"),
        vec!["first run".to_string(), "after first run".to_string()]
    );
    assert_eq!(
        session
            .state()
            .latest_run
            .unwrap()
            .result
            .final_answer
            .as_deref(),
        Some("after first run")
    );
}

#[test]
fn session_continue_run_uses_queued_prompt_without_auto_follow_up() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let execute_run = {
        let calls = Arc::clone(&calls);
        Arc::new(move |prompt: String| {
            calls.lock().expect("calls").push(prompt.clone());
            Ok(fake_run(&prompt, AgentStatus::Completed))
        })
    };
    let mut session = AgentSession::new(
        execute_run,
        "demo",
        AgentDefinition::default_for_model("demo"),
        "./workspace",
    );

    session.follow_up("queued follow-up").expect("follow_up");
    let run = session.continue_run(None).expect("continue");

    assert_eq!(run.result.final_answer.as_deref(), Some("queued follow-up"));
    assert_eq!(
        *calls.lock().expect("calls"),
        vec!["queued follow-up".to_string()]
    );
}

#[test]
fn session_query_raises_when_not_completed() {
    let execute_run = Arc::new(move |prompt: String| Ok(fake_run(&prompt, AgentStatus::WaitUser)));
    let mut session = AgentSession::new(
        execute_run,
        "demo",
        AgentDefinition::default_for_model("demo"),
        "./workspace",
    );

    let error = session.query("ask").expect_err("query error");

    assert!(error.contains("status=wait_user"));
}

#[test]
fn session_emits_queue_and_run_events() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let execute_run = {
        let calls = Arc::clone(&calls);
        Arc::new(move |prompt: String| {
            calls.lock().expect("calls").push(prompt.clone());
            Ok(fake_run(&prompt, AgentStatus::Completed))
        })
    };
    let mut session = AgentSession::new(
        execute_run,
        "demo",
        AgentDefinition::default_for_model("demo"),
        "./workspace",
    );
    let events = recorded_events();
    session.subscribe(recording_listener(&events));

    session.follow_up("after first").expect("follow_up");
    let run = session.prompt("first").expect("prompt");

    assert_eq!(run.result.final_answer.as_deref(), Some("after first"));
    let events = events.lock().expect("events");
    let event_names: Vec<&str> = events.iter().map(|(event, _)| event.as_str()).collect();
    assert_eq!(
        event_names,
        vec![
            "session_follow_up_queued",
            "session_run_start",
            "session_run_end",
            "session_follow_up_dequeued",
            "session_run_start",
            "session_run_end",
        ]
    );
    assert_eq!(events[1].1["prompt"], Value::String("first".to_string()));
    assert_eq!(events[1].1["existing_messages"], Value::from(0));
    assert_eq!(
        events[2].1["status"],
        Value::String("completed".to_string())
    );
    assert_eq!(
        events[3].1["prompt"],
        Value::String("after first".to_string())
    );
}

#[test]
fn session_unsubscribe_removes_listener() {
    let execute_run = Arc::new(move |prompt: String| Ok(fake_run(&prompt, AgentStatus::Completed)));
    let mut session = AgentSession::new(
        execute_run,
        "demo",
        AgentDefinition::default_for_model("demo"),
        "./workspace",
    );
    let events = recorded_events();
    let listener_id = session.subscribe(recording_listener(&events));

    assert!(session.unsubscribe(listener_id));
    session.follow_up("silent").expect("follow_up");

    assert!(events.lock().expect("events").is_empty());
}

#[test]
fn session_clear_queues_emits_event_and_drops_prompts() {
    let execute_run = Arc::new(move |prompt: String| Ok(fake_run(&prompt, AgentStatus::Completed)));
    let mut session = AgentSession::new(
        execute_run,
        "demo",
        AgentDefinition::default_for_model("demo"),
        "./workspace",
    );
    let events = recorded_events();
    session.subscribe(recording_listener(&events));

    session.steer("urgent").expect("steer");
    session.follow_up("later").expect("follow_up");
    session.clear_queues();
    let error = session.continue_run(None).expect_err("empty queue");

    assert!(error.contains("No queued prompt available"));
    let events = events.lock().expect("events");
    let event_names: Vec<&str> = events.iter().map(|(event, _)| event.as_str()).collect();
    assert_eq!(
        event_names,
        vec![
            "session_steer_queued",
            "session_follow_up_queued",
            "session_queues_cleared",
        ]
    );
}

#[test]
fn session_cancel_requests_active_runtime_and_clears_queues() {
    let client = AgentSDKClient::new(AgentSDKOptions::default()).with_runtime(AgentRuntime::new(
        ScriptedLlmClient::new(vec![LLMResponse::new("should not be used")]),
    ));
    let mut session =
        create_agent_session(&client, "demo", AgentDefinition::default_for_model("demo"));
    let cancellation = session.cancellation_handle();
    let events = recorded_events();
    session.subscribe(recording_listener(&events));
    session.subscribe(Arc::new(move |event, _payload| {
        if event == "session_run_start" {
            assert!(cancellation.cancel());
        }
    }));

    session.follow_up("later").expect("follow_up");
    session.steer("urgent").expect("steer");
    let run = session.prompt("start").expect("prompt");

    assert_eq!(run.result.status, AgentStatus::Failed);
    assert!(run
        .result
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("cancelled"));
    assert!(run.result.cycles.is_empty());
    assert!(!session.cancel());
    assert!(session
        .continue_run(None)
        .expect_err("queues cleared")
        .contains("No queued prompt available"));

    let events = events.lock().expect("events");
    assert!(events
        .iter()
        .any(|(event, _)| event == "session_cancel_requested"));
    assert!(events.iter().any(|(event, _)| event == "session_run_end"));
}

#[test]
fn session_run_to_dict_contains_structured_token_usage() {
    let mut run = fake_run("ok", AgentStatus::Completed);
    run.resolved = ResolvedModelConfig::new(
        "deepseek",
        "deepseek-v4-pro",
        "deepseek-v4-pro",
        "deepseek-chat",
        vec![vv_agent::EndpointOption::new(
            vv_agent::EndpointConfig::new("primary", "secret", "https://api.example.test"),
            "deepseek-chat",
        )],
    );
    run.result = vv_agent::AgentResult::completed(
        vec![],
        vec![CycleRecord {
            index: 3,
            assistant_message: "ok".to_string(),
            tool_calls: vec![],
            tool_results: vec![],
            memory_compacted: false,
            token_usage: TokenUsage {
                prompt_tokens: 11,
                completion_tokens: 7,
                total_tokens: 18,
                cached_tokens: 2,
                reasoning_tokens: 5,
                ..TokenUsage::default()
            },
        }],
        "ok",
    );
    run.result.wait_reason = Some("not needed".to_string());
    run.result.error = Some("none".to_string());
    run.result.shared_state.insert(
        "todo_list".to_string(),
        serde_json::json!([{"id": "t1", "title": "done", "status": "completed"}]),
    );

    let payload = run.to_dict();
    let usage = payload.get("token_usage").expect("token_usage");

    assert_eq!(
        payload["wait_reason"],
        Value::String("not needed".to_string())
    );
    assert_eq!(payload["error"], Value::String("none".to_string()));
    assert_eq!(
        payload["todo_list"][0]["id"],
        Value::String("t1".to_string())
    );
    assert_eq!(
        payload["resolved"]["backend"],
        Value::String("deepseek".to_string())
    );
    assert_eq!(
        payload["resolved"]["selected_model"],
        Value::String("deepseek-v4-pro".to_string())
    );
    assert_eq!(
        payload["resolved"]["model_id"],
        Value::String("deepseek-chat".to_string())
    );
    assert_eq!(
        payload["resolved"]["endpoint"],
        Value::String("primary".to_string())
    );
    assert_eq!(usage["prompt_tokens"], Value::from(11));
    assert_eq!(usage["completion_tokens"], Value::from(7));
    assert_eq!(usage["total_tokens"], Value::from(18));
    assert_eq!(usage["cached_tokens"], Value::from(2));
    assert_eq!(usage["reasoning_tokens"], Value::from(5));
    assert_eq!(usage["cycles"][0]["cycle_index"], Value::from(3));
    assert_eq!(usage["cycles"][0]["usage"]["total_tokens"], Value::from(18));
}

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
    assert!(session_id.starts_with("session-"));
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

#[test]
fn sdk_runtime_uses_options_workspace_for_tool_context_and_sessions() {
    let workspace = tempfile::tempdir().expect("workspace");
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
    let workspaces = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = AgentRuntime::new(ScriptedLlmClient::new(responses));
    runtime.hooks.push(Arc::new(WorkspaceRecordingHook {
        workspaces: Arc::clone(&workspaces),
    }));
    let client = AgentSDKClient::new(AgentSDKOptions {
        workspace: workspace.path().to_path_buf(),
        ..AgentSDKOptions::default()
    })
    .with_runtime(runtime);
    let session = create_agent_session(&client, "demo", AgentDefinition::default_for_model("demo"));

    let run = client
        .run_with_agent(AgentDefinition::default_for_model("demo"), "finish")
        .expect("run");

    assert_eq!(run.result.status, AgentStatus::Completed);
    assert_eq!(session.state().workspace, workspace.path());
    assert_eq!(
        workspaces.lock().expect("workspaces").as_slice(),
        &[workspace.path().to_path_buf()]
    );
}

#[test]
fn sdk_runtime_applies_startup_shell_defaults_to_tool_context_like_python() {
    let responses = vec![
        LLMResponse {
            content: "run shell".to_string(),
            tool_calls: vec![ToolCall::new(
                "bash-1",
                "bash",
                json_args(serde_json::json!({"command": "echo skipped"})),
            )],
            raw: BTreeMap::new(),
            token_usage: TokenUsage::default(),
        },
        LLMResponse {
            content: "finish".to_string(),
            tool_calls: vec![ToolCall::new(
                "finish-1",
                "task_finish",
                json_args(serde_json::json!({"message": "ok"})),
            )],
            raw: BTreeMap::new(),
            token_usage: TokenUsage::default(),
        },
    ];
    let captured_metadata = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = AgentRuntime::new(ScriptedLlmClient::new(responses));
    runtime.hooks.push(Arc::new(ShellMetadataCaptureHook {
        captured_metadata: Arc::clone(&captured_metadata),
    }));
    let mut client = AgentSDKClient::new(AgentSDKOptions {
        bash_shell: Some("powershell".to_string()),
        windows_shell_priority: vec!["git-bash".to_string(), "powershell".to_string()],
        bash_env: BTreeMap::from([
            (
                "VV_AGENT_OPTION_ONLY".to_string(),
                "from-option".to_string(),
            ),
            ("VV_AGENT_SHARED".to_string(), "from-option".to_string()),
        ]),
        ..AgentSDKOptions::default()
    })
    .with_runtime(runtime);
    let mut definition = AgentDefinition::default_for_model("demo");
    definition.extra_tool_names = vec!["bash".to_string()];
    definition.bash_env = BTreeMap::from([
        ("VV_AGENT_AGENT_ONLY".to_string(), "from-agent".to_string()),
        ("VV_AGENT_SHARED".to_string(), "from-agent".to_string()),
    ]);
    client.set_default_agent(definition);

    let run = client.query("run shell").expect("query");

    assert_eq!(run, "ok");
    let captured = captured_metadata.lock().expect("captured metadata");
    let metadata = captured.first().expect("bash metadata");
    assert_eq!(metadata["bash_shell"], "powershell");
    assert_eq!(
        metadata["windows_shell_priority"],
        serde_json::json!(["git-bash", "powershell"])
    );
    assert_eq!(metadata["bash_env"]["VV_AGENT_OPTION_ONLY"], "from-option");
    assert_eq!(metadata["bash_env"]["VV_AGENT_AGENT_ONLY"], "from-agent");
    assert_eq!(metadata["bash_env"]["VV_AGENT_SHARED"], "from-agent");
}

#[test]
fn sdk_client_query_reports_wait_user_status() {
    let responses = vec![LLMResponse {
        content: "ask".to_string(),
        tool_calls: vec![ToolCall::new(
            "ask-1",
            "ask_user",
            json_args(serde_json::json!({"question": "choose one"})),
        )],
        raw: BTreeMap::new(),
        token_usage: TokenUsage::default(),
    }];
    let mut client = AgentSDKClient::new(AgentSDKOptions::default())
        .with_runtime(AgentRuntime::new(ScriptedLlmClient::new(responses)));
    client.set_default_agent(AgentDefinition::default_for_model("demo"));

    let error = client.query("ask").expect_err("query error");

    assert!(error.contains("status=wait_user"));
    assert!(error.contains("choose one"));
}

fn fake_run(prompt: &str, status: AgentStatus) -> AgentRun {
    let mut result = vv_agent::AgentResult::completed(vec![], vec![], prompt.to_string());
    result.status = status;
    if status == AgentStatus::WaitUser {
        result.wait_reason = Some("need input".to_string());
        result.final_answer = None;
    }
    AgentRun {
        agent_name: "demo".to_string(),
        result,
        resolved: ResolvedModelConfig::new("demo", "demo", "demo", "demo", vec![]),
    }
}

type RecordedEvents = Arc<Mutex<Vec<(String, BTreeMap<String, Value>)>>>;

struct ShellMetadataCaptureHook {
    captured_metadata: Arc<Mutex<Vec<BTreeMap<String, Value>>>>,
}

struct TaskMetadataCaptureHook {
    captured_metadata: Arc<Mutex<Vec<BTreeMap<String, Value>>>>,
}

impl RuntimeHook for TaskMetadataCaptureHook {
    fn before_llm(&self, event: BeforeLlmEvent<'_>) -> Option<vv_agent::BeforeLlmPatch> {
        self.captured_metadata
            .lock()
            .expect("captured metadata")
            .push(event.task.metadata.clone());
        None
    }
}

impl RuntimeHook for ShellMetadataCaptureHook {
    fn before_tool_call(&self, event: BeforeToolCallEvent<'_>) -> Option<BeforeToolCallPatch> {
        if event.call.name != "bash" {
            return None;
        }
        self.captured_metadata
            .lock()
            .expect("captured metadata")
            .push(event.context.metadata.clone());
        let mut result = ToolExecutionResult::success(event.call.id.clone(), "{}");
        result.directive = ToolDirective::Continue;
        Some(BeforeToolCallPatch {
            call: None,
            result: Some(result),
        })
    }
}

fn recorded_events() -> RecordedEvents {
    Arc::new(Mutex::new(Vec::new()))
}

fn recording_listener(events: &RecordedEvents) -> SessionEventHandler {
    let events = Arc::clone(events);
    Arc::new(move |event, payload| {
        events
            .lock()
            .expect("events")
            .push((event.to_string(), payload.clone()));
    })
}

#[derive(Debug)]
struct RuntimeSnapshot {
    messages: Vec<String>,
    shared_state: BTreeMap<String, Value>,
}

struct RecordingRuntimeHook {
    snapshots: Arc<Mutex<Vec<RuntimeSnapshot>>>,
}

impl RuntimeHook for RecordingRuntimeHook {
    fn before_llm(&self, event: BeforeLlmEvent<'_>) -> Option<vv_agent::BeforeLlmPatch> {
        self.snapshots
            .lock()
            .expect("snapshots")
            .push(RuntimeSnapshot {
                messages: event
                    .messages
                    .iter()
                    .map(|message| {
                        format!("{:?}:{}", message.role, message.content).to_ascii_lowercase()
                    })
                    .collect(),
                shared_state: event.shared_state.clone(),
            });
        None
    }
}

struct WorkspaceRecordingHook {
    workspaces: Arc<Mutex<Vec<std::path::PathBuf>>>,
}

impl RuntimeHook for WorkspaceRecordingHook {
    fn before_tool_call(&self, event: BeforeToolCallEvent<'_>) -> Option<BeforeToolCallPatch> {
        self.workspaces
            .lock()
            .expect("workspaces")
            .push(event.context.workspace.clone());
        None
    }
}

fn json_args(value: Value) -> BTreeMap<String, Value> {
    value
        .as_object()
        .expect("object args")
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}
