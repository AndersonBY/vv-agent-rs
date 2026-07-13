#[tokio::test]
async fn active_run_accepts_steering_and_runs_queued_follow_up() {
    let tool_started = Arc::new(tokio::sync::Notify::new());
    let release_tool = Arc::new(tokio::sync::Notify::new());
    let requests = Arc::new(Mutex::new(Vec::<LlmRequest>::new()));
    let steps = (0..3)
        .map(|index| {
            let requests = requests.clone();
            ScriptStep::callback(move |request| {
                requests.lock().expect("requests").push(request.clone());
                Ok(match index {
                    0 => LLMResponse::with_tool_calls(
                        "working",
                        vec![ToolCall::new("call_1", "slow_tool", BTreeMap::new())],
                    ),
                    1 => finish_response("first done"),
                    _ => finish_response("follow-up done"),
                })
            })
        })
        .collect();
    let provider = ScriptedModelProvider::from_steps("scripted", "demo-model", steps);
    let started_for_tool = tool_started.clone();
    let release_for_tool = release_tool.clone();
    let slow_tool = FunctionTool::builder("slow_tool")
        .description("Wait for the test to release this tool.")
        .json_schema(json!({"type": "object", "properties": {}, "required": []}))
        .handler(move |_context, _arguments: Value| {
            let started = started_for_tool.clone();
            let release = release_for_tool.clone();
            async move {
                started.notify_one();
                release.notified().await;
                Ok(ToolOutput::text("released"))
            }
        })
        .build()
        .expect("slow tool");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Use the tool, incorporate steering, then finish.")
        .model(ModelRef::named("demo-model"))
        .tool(slow_tool)
        .build()
        .expect("agent");
    let session = InteractiveAgentClient::new(runner)
        .create_session(
            agent,
            InteractiveSessionOptions::new().session_id("interactive-steering"),
        )
        .await
        .expect("session");
    let session_for_prompt = session.clone();
    let prompt_task = tokio::spawn(async move { session_for_prompt.prompt("initial").await });

    tokio::time::timeout(Duration::from_secs(5), tool_started.notified())
        .await
        .expect("tool start timeout");
    assert!(session.running());
    assert!(session.active_run_handle().is_some());
    let concurrent_error = match session.prompt("second direct prompt").await {
        Ok(_) => panic!("concurrent prompt must not start"),
        Err(error) => error,
    };
    assert!(matches!(
        concurrent_error,
        InteractiveSessionError::AlreadyRunning { ref session_id }
            if session_id == "interactive-steering"
    ));
    let active_handle = session.active_run_handle().expect("active handle");
    active_handle
        .steer("steered through handle")
        .expect("handle steering");
    session
        .steer("steered through session")
        .expect("session steering");
    active_handle
        .follow_up("continue")
        .expect("handle follow-up");
    assert_eq!(session.state().pending_steering, 2);
    assert_eq!(session.state().pending_follow_ups, 1);
    release_tool.notify_one();

    let result = tokio::time::timeout(Duration::from_secs(5), prompt_task)
        .await
        .expect("prompt timeout")
        .expect("prompt task")
        .expect("prompt result");

    assert_eq!(result.final_output(), Some("follow-up done"));
    assert!(!session.running());
    assert!(session.active_run_handle().is_none());
    let requests = requests.lock().expect("requests");
    assert_eq!(requests.len(), 3);
    assert!(requests[1]
        .messages
        .iter()
        .any(|message| message.content == "steered through handle"));
    assert!(requests[1]
        .messages
        .iter()
        .any(|message| message.content == "steered through session"));
    assert!(requests[2]
        .messages
        .iter()
        .any(|message| message.content == "continue"));
    assert!(active_handle.steer("late").is_err());
    assert!(active_handle.follow_up("late").is_err());
}

#[tokio::test]
async fn steering_drains_fifo_and_skips_not_started_tools_as_errors() {
    let tool_started = Arc::new(tokio::sync::Notify::new());
    let release_tool = Arc::new(tokio::sync::Notify::new());
    let skipped_tool_calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::<LlmRequest>::new()));
    let captured_requests = requests.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "demo-model",
        vec![
            ScriptStep::callback(move |request| {
                captured_requests
                    .lock()
                    .expect("requests")
                    .push(request.clone());
                Ok(LLMResponse::with_tool_calls(
                    "two tools",
                    vec![
                        ToolCall::new("slow", "slow_tool", BTreeMap::new()),
                        ToolCall::new("never", "never_tool", BTreeMap::new()),
                    ],
                ))
            }),
            {
                let requests = requests.clone();
                ScriptStep::callback(move |request| {
                    requests.lock().expect("requests").push(request.clone());
                    Ok(finish_response("done"))
                })
            },
        ],
    );
    let started = tool_started.clone();
    let release = release_tool.clone();
    let slow_tool = FunctionTool::builder("slow_tool")
        .description("Wait for steering.")
        .json_schema(json!({"type": "object", "properties": {}, "required": []}))
        .handler(move |_context, _arguments: Value| {
            let started = started.clone();
            let release = release.clone();
            async move {
                started.notify_one();
                release.notified().await;
                Ok(ToolOutput::text("released"))
            }
        })
        .build()
        .expect("slow tool");
    let never_calls = skipped_tool_calls.clone();
    let never_tool = FunctionTool::builder("never_tool")
        .description("Must be skipped after steering.")
        .json_schema(json!({"type": "object", "properties": {}, "required": []}))
        .handler(move |_context, _arguments: Value| {
            never_calls.fetch_add(1, Ordering::SeqCst);
            async { Ok(ToolOutput::text("unexpected")) }
        })
        .build()
        .expect("never tool");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Use tools and follow steering.")
        .model(ModelRef::named("demo-model"))
        .tool(slow_tool)
        .tool(never_tool)
        .build()
        .expect("agent");
    let session = InteractiveAgentClient::new(runner)
        .create_session(
            agent,
            InteractiveSessionOptions::new().session_id("steering-fifo"),
        )
        .await
        .expect("session");
    let prompt_session = session.clone();
    let prompt_task = tokio::spawn(async move { prompt_session.prompt_once("initial").await });

    tokio::time::timeout(Duration::from_secs(5), tool_started.notified())
        .await
        .expect("tool start timeout");
    session.steer("first steer").expect("first steer");
    session.steer("second steer").expect("second steer");
    release_tool.notify_one();

    let result = prompt_task
        .await
        .expect("prompt task")
        .expect("prompt result");
    assert_eq!(result.final_output(), Some("done"));
    assert_eq!(skipped_tool_calls.load(Ordering::SeqCst), 0);
    let skipped = &result.result().cycles[0].tool_results[1];
    assert_eq!(skipped.status, ToolResultStatus::Error);
    assert_eq!(
        skipped.error_code.as_deref(),
        Some("skipped_due_to_steering")
    );
    let requests = requests.lock().expect("requests");
    let steered = requests[1]
        .messages
        .iter()
        .filter(|message| message.role == vv_agent::MessageRole::User)
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>();
    assert!(steered.ends_with(&["first steer", "second steer"]));
    assert_eq!(session.state().pending_steering, 0);
}

#[tokio::test]
async fn cancel_targets_active_handle_and_clears_queued_work() {
    let tool_started = Arc::new(tokio::sync::Notify::new());
    let release_tool = Arc::new(tokio::sync::Notify::new());
    let started_for_tool = tool_started.clone();
    let release_for_tool = release_tool.clone();
    let slow_tool = FunctionTool::builder("slow_tool")
        .description("Wait for cancellation.")
        .json_schema(json!({"type": "object", "properties": {}, "required": []}))
        .handler(move |_context, _arguments: Value| {
            let started = started_for_tool.clone();
            let release = release_for_tool.clone();
            async move {
                started.notify_one();
                release.notified().await;
                Ok(ToolOutput::text("released"))
            }
        })
        .build()
        .expect("slow tool");
    let runner = scripted_runner(vec![
        LLMResponse::with_tool_calls(
            "working",
            vec![ToolCall::new("call_1", "slow_tool", BTreeMap::new())],
        ),
        finish_response("should not continue"),
    ]);
    let agent = Agent::builder("assistant")
        .instructions("Use the tool, then finish.")
        .model(ModelRef::named("demo-model"))
        .tool(slow_tool)
        .build()
        .expect("agent");
    let parent_cancellation = vv_agent::CancellationToken::default();
    let session = InteractiveAgentClient::new(runner)
        .create_session(
            agent,
            InteractiveSessionOptions::new()
                .session_id("interactive-cancel")
                .run_config(
                    vv_agent::RunConfig::builder()
                        .cancellation_token(parent_cancellation.clone())
                        .build(),
                ),
        )
        .await
        .expect("session");
    let mut events = session.subscribe();
    let session_for_prompt = session.clone();
    let prompt_task = tokio::spawn(async move { session_for_prompt.prompt("initial").await });

    tokio::time::timeout(Duration::from_secs(5), tool_started.notified())
        .await
        .expect("tool start timeout");
    session.follow_up("do not run").expect("follow-up");
    let handle = session.active_run_handle().expect("active handle");
    assert!(session.cancel());
    assert!(!parent_cancellation.is_cancelled());
    let accepted = handle.state();
    assert_eq!(accepted.status, RunHandleStatus::Running);
    assert!(!accepted.done);
    assert!(accepted.cancelled);
    assert_eq!(session.state().pending_follow_ups, 0);
    assert_eq!(session.state().pending_steering, 0);
    release_tool.notify_one();

    let result = tokio::time::timeout(Duration::from_secs(5), prompt_task)
        .await
        .expect("prompt timeout")
        .expect("prompt task")
        .expect("prompt result");

    assert_eq!(result.status(), AgentStatus::Failed);
    assert!(!session.cancel());
    assert!(drain_events(&mut events)
        .iter()
        .any(|event| matches!(event, InteractiveSessionEvent::CancelRequested { .. })));

    let retry = session
        .prompt_once("retry")
        .await
        .expect("retry after cancel");
    assert_eq!(retry.status(), AgentStatus::Completed);
    assert_eq!(retry.final_output(), Some("should not continue"));
}
