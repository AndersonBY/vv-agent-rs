#[tokio::test]
async fn parent_cancellation_propagates_to_the_active_interactive_run() {
    let tool_started = Arc::new(tokio::sync::Notify::new());
    let release_tool = Arc::new(tokio::sync::Notify::new());
    let started = tool_started.clone();
    let release = release_tool.clone();
    let slow_tool = FunctionTool::builder("slow_tool")
        .description("Wait for parent cancellation.")
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
    let runner = scripted_runner(vec![LLMResponse::with_tool_calls(
        "working",
        vec![ToolCall::new("call_1", "slow_tool", BTreeMap::new())],
    )]);
    let agent = Agent::builder("assistant")
        .instructions("Use the tool.")
        .model(ModelRef::named("demo-model"))
        .tool(slow_tool)
        .build()
        .expect("agent");
    let parent = vv_agent::CancellationToken::default();
    let session = InteractiveAgentClient::new(runner)
        .create_session(
            agent,
            InteractiveSessionOptions::new()
                .session_id("parent-cancellation")
                .run_config(
                    vv_agent::RunConfig::builder()
                        .cancellation_token(parent.clone())
                        .build(),
                ),
        )
        .await
        .expect("session");
    let prompt_session = session.clone();
    let prompt_task = tokio::spawn(async move { prompt_session.prompt_once("initial").await });

    tokio::time::timeout(Duration::from_secs(5), tool_started.notified())
        .await
        .expect("tool start timeout");
    parent.cancel();
    release_tool.notify_one();

    let result = prompt_task
        .await
        .expect("prompt task")
        .expect("prompt result");
    assert_eq!(result.status(), AgentStatus::Failed);
    assert!(parent.is_cancelled());
    assert!(!session.cancel());
}

#[tokio::test]
async fn pull_subscribers_are_independent_and_report_bounded_gaps() {
    let session = InteractiveAgentClient::new(scripted_runner(vec![]))
        .create_session(
            scripted_agent(),
            InteractiveSessionOptions::new()
                .session_id("subscriber-gap")
                .event_buffer_capacity(2),
        )
        .await
        .expect("session");
    let mut fast = session.subscribe();
    let mut slow = session.subscribe();

    session.steer("one").expect("first event");
    assert!(matches!(
        fast.try_recv(),
        Ok(InteractiveSessionEvent::SteerQueued { prompt, .. }) if prompt == "one"
    ));
    session.steer("two").expect("second event");
    assert!(matches!(
        fast.try_recv(),
        Ok(InteractiveSessionEvent::SteerQueued { prompt, .. }) if prompt == "two"
    ));
    session.steer("three").expect("third event");
    assert!(matches!(
        fast.try_recv(),
        Ok(InteractiveSessionEvent::SteerQueued { prompt, .. }) if prompt == "three"
    ));

    assert!(matches!(
        slow.recv().await,
        Err(InteractiveSessionError::EventGap { missed: 1 })
    ));
    assert!(matches!(
        slow.recv().await,
        Ok(InteractiveSessionEvent::SteerQueued { prompt, .. }) if prompt == "two"
    ));
    assert!(matches!(
        slow.recv().await,
        Ok(InteractiveSessionEvent::SteerQueued { prompt, .. }) if prompt == "three"
    ));
}

#[tokio::test]
async fn automatic_follow_ups_remain_fifo_and_prompt_once_preserves_the_queue() {
    let requests = Arc::new(Mutex::new(Vec::<LlmRequest>::new()));
    let steps = ["initial", "first", "second"]
        .into_iter()
        .map(|answer| {
            let requests = requests.clone();
            ScriptStep::callback(move |request| {
                requests.lock().expect("requests").push(request.clone());
                Ok(finish_response(answer))
            })
        })
        .collect();
    let provider = ScriptedModelProvider::from_steps("scripted", "demo-model", steps);
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .build()
        .expect("runner");
    let session = InteractiveAgentClient::new(runner)
        .create_session(
            scripted_agent(),
            InteractiveSessionOptions::new().session_id("follow-up-fifo"),
        )
        .await
        .expect("session");
    session
        .follow_up("first follow-up")
        .expect("first follow-up");
    session
        .follow_up("second follow-up")
        .expect("second follow-up");

    let first = session
        .prompt_once("initial prompt")
        .await
        .expect("first turn");
    assert_eq!(first.final_output(), Some("initial"));
    assert_eq!(session.state().pending_follow_ups, 2);

    let last = session.continue_run(None).await.expect("first queued turn");
    assert_eq!(last.final_output(), Some("first"));
    assert_eq!(session.state().pending_follow_ups, 1);
    let last = session
        .continue_run(None)
        .await
        .expect("second queued turn");
    assert_eq!(last.final_output(), Some("second"));
    assert_eq!(session.state().pending_follow_ups, 0);

    let requests = requests.lock().expect("requests");
    let final_user_messages = requests
        .iter()
        .map(|request| {
            request
                .messages
                .iter()
                .rev()
                .find(|message| message.role == vv_agent::MessageRole::User)
                .map(|message| message.content.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        final_user_messages,
        ["initial prompt", "first follow-up", "second follow-up"]
    );
}

#[tokio::test]
async fn background_completion_queues_a_notification_for_the_next_turn() {
    let workspace = tempfile::tempdir().expect("workspace");
    let requests = Arc::new(Mutex::new(Vec::<LlmRequest>::new()));
    let captured = requests.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "demo-model",
        vec![
            ScriptStep::callback(move |request| {
                captured.lock().expect("requests").push(request.clone());
                Ok(LLMResponse::with_tool_calls(
                    "launch",
                    vec![ToolCall::new(
                        "background",
                        "bash",
                        BTreeMap::from([
                            ("command".to_string(), json!("sleep 1; printf bridge-ready")),
                            ("run_in_background".to_string(), json!(true)),
                            ("timeout".to_string(), json!(5)),
                        ]),
                    )],
                ))
            }),
            {
                let requests = requests.clone();
                ScriptStep::callback(move |request| {
                    requests.lock().expect("requests").push(request.clone());
                    Ok(finish_response("launched"))
                })
            },
            {
                let requests = requests.clone();
                ScriptStep::callback(move |request| {
                    requests.lock().expect("requests").push(request.clone());
                    Ok(finish_response("observed"))
                })
            },
        ],
    );
    let defaults = build_default_registry();
    let bash_spec = defaults.get("bash").expect("default bash tool").clone();
    let mut registry = ToolRegistry::new();
    registry
        .register(
            defaults
                .get("task_finish")
                .expect("default task_finish tool")
                .clone(),
        )
        .expect("minimal task_finish registry");
    let bash = StaticTool::new(
        bash_spec.name,
        bash_spec.description,
        bash_spec.schema["function"]["parameters"].clone(),
        bash_spec.handler,
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(workspace.path())
        .tool_registry(registry)
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Launch the command, then consume its completion notification.")
        .model(ModelRef::named("demo-model"))
        .tool(bash)
        .build()
        .expect("agent");
    let session = InteractiveAgentClient::new(runner)
        .create_session(
            agent,
            InteractiveSessionOptions::new()
                .session_id("background-bridge")
                .event_buffer_capacity(64),
        )
        .await
        .expect("session");
    let mut events = session.subscribe();

    let launched = session.prompt_once("launch it").await.expect("launch turn");
    assert_eq!(launched.final_output(), Some("launched"));
    let notification = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Ok(InteractiveSessionEvent::BackgroundCommandTerminal {
                    status,
                    notification_message,
                    queued_to_session,
                    queued_to_running_session,
                    ..
                }) => {
                    assert_eq!(status, "completed");
                    assert!(!queued_to_session);
                    assert!(!queued_to_running_session);
                    break notification_message;
                }
                Ok(_) => {}
                Err(error) => panic!("interactive event stream failed: {error}"),
            }
        }
    })
    .await
    .expect("background completion timeout");

    assert_eq!(session.state().pending_steering, 0);
    let observed = session
        .prompt_once(notification)
        .await
        .expect("notification turn");
    assert_eq!(observed.final_output(), Some("observed"));
    assert_eq!(session.state().pending_steering, 0);
    assert!(requests
        .lock()
        .expect("requests")
        .last()
        .expect("notification request")
        .messages
        .iter()
        .any(|message| {
            message.role == vv_agent::MessageRole::User
                && message
                    .content
                    .starts_with("System notification: background command bg_")
                && message.content.contains("Summary: bridge-ready")
        }));
}
