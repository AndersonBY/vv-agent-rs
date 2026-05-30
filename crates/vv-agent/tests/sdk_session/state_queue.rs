use super::*;

#[test]
fn session_state_starts_with_agent_todo_list() {
    let execute_run = Arc::new(|prompt: String| Ok(fake_run(&prompt, AgentStatus::Completed)));
    let session = AgentSession::new(
        execute_run,
        "demo",
        AgentDefinition::default_for_model("demo"),
        "./workspace",
    );

    assert_eq!(
        session.state().shared_state["todo_list"],
        Value::Array(vec![])
    );
}

#[test]
fn session_id_defaults_to_agent_hex_prefix() {
    let execute_run = Arc::new(|prompt: String| Ok(fake_run(&prompt, AgentStatus::Completed)));
    let session = AgentSession::new(
        execute_run,
        "demo",
        AgentDefinition::default_for_model("demo"),
        "./workspace",
    );

    assert_eq!(session.session_id().len(), 12);
    assert!(session
        .session_id()
        .chars()
        .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()));
}

#[test]
fn session_constructor_preserves_initial_shared_state() {
    let execute_run = Arc::new(|request: vv_agent::AgentSessionRunRequest| {
        assert_eq!(
            request.shared_state.get("seed").and_then(Value::as_str),
            Some("from-session")
        );
        Ok(AgentRun {
            agent_name: "demo-agent".to_string(),
            result: vv_agent::AgentResult::completed_with_shared_state(
                vec![vv_agent::Message::user(request.prompt.clone())],
                vec![],
                request.prompt,
                request.shared_state,
            ),
            resolved: ResolvedModelConfig::new("demo", "demo", "demo", "demo", vec![]),
        })
    });
    let mut session = AgentSession::new_with_context_and_shared_state(
        execute_run,
        "demo-agent",
        AgentDefinition::default_for_model("demo-model"),
        "./workspace",
        BTreeMap::from([(
            "seed".to_string(),
            Value::String("from-session".to_string()),
        )]),
    );

    assert_eq!(
        session.shared_state().get("seed").and_then(Value::as_str),
        Some("from-session")
    );
    assert_eq!(session.shared_state()["todo_list"], Value::Array(vec![]));

    let run = session.prompt("hello").expect("prompt");

    assert_eq!(
        run.result.shared_state.get("seed").and_then(Value::as_str),
        Some("from-session")
    );
}

#[test]
fn create_agent_session_helper_accepts_initial_shared_state() {
    let client = AgentSDKClient::new(AgentSDKOptions {
        auto_discover_resources: false,
        ..AgentSDKOptions::default()
    });
    let session = create_agent_session_with_shared_state(
        &client,
        "demo",
        AgentDefinition::default_for_model("demo-model"),
        BTreeMap::from([("seed".to_string(), Value::String("from-helper".to_string()))]),
    );

    assert_eq!(
        session.shared_state().get("seed").and_then(Value::as_str),
        Some("from-helper")
    );
    assert_eq!(session.shared_state()["todo_list"], Value::Array(vec![]));
}

#[test]
fn session_exposes_agent_state_accessors() {
    let workspace = tempfile::tempdir().expect("workspace");
    let execute_run = Arc::new(|prompt: String| {
        Ok(AgentRun {
            agent_name: "demo-agent".to_string(),
            result: vv_agent::AgentResult::completed(
                vec![vv_agent::Message::user(prompt.clone())],
                vec![],
                prompt,
            ),
            resolved: ResolvedModelConfig::new("demo", "demo", "demo", "demo", vec![]),
        })
    });
    let mut definition = AgentDefinition::default_for_model("demo-model");
    definition.description = "Demo session agent".to_string();
    let mut session = AgentSession::new(
        execute_run,
        "demo-agent",
        definition.clone(),
        workspace.path(),
    );

    assert_eq!(session.agent_name(), "demo-agent");
    assert_eq!(session.definition(), &definition);
    assert_eq!(session.workspace(), workspace.path());
    assert!(!session.running());
    assert_eq!(session.messages(), Vec::new());
    assert_eq!(session.shared_state()["todo_list"], Value::Array(vec![]));
    assert!(session.latest_run().is_none());

    session.prompt("hello").expect("prompt");

    assert!(!session.running());
    assert_eq!(session.messages()[0].content, "hello");
    assert_eq!(
        session.latest_run().unwrap().result.final_answer.as_deref(),
        Some("hello")
    );
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
