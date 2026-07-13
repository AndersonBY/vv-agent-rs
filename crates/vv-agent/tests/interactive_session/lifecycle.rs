#[tokio::test]
async fn aborting_prompt_future_cleans_active_handle_and_emits_one_abort() {
    let tool_started = Arc::new(tokio::sync::Notify::new());
    let release_tool = Arc::new(tokio::sync::Notify::new());
    let started = tool_started.clone();
    let release = release_tool.clone();
    let slow_tool = FunctionTool::builder("slow_tool")
        .description("Wait until the prompt future is aborted.")
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
        .instructions("Use the slow tool.")
        .model(ModelRef::named("demo-model"))
        .tool(slow_tool)
        .build()
        .expect("agent");
    let session = InteractiveAgentClient::new(runner)
        .create_session(
            agent,
            InteractiveSessionOptions::new().session_id("future-abort"),
        )
        .await
        .expect("session");
    let mut events = session.subscribe();
    let prompt_session = session.clone();
    let prompt_task = tokio::spawn(async move { prompt_session.prompt_once("initial").await });

    tokio::time::timeout(Duration::from_secs(5), tool_started.notified())
        .await
        .expect("tool start timeout");
    assert!(session.active_run_handle().is_some());
    prompt_task.abort();
    let join_error = match prompt_task.await {
        Ok(_) => panic!("prompt task must be aborted"),
        Err(error) => error,
    };
    assert!(join_error.is_cancelled());
    release_tool.notify_one();
    tokio::task::yield_now().await;

    assert!(!session.running());
    assert!(session.active_run_handle().is_none());
    let events = drain_events(&mut events);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, InteractiveSessionEvent::RunAborted { .. }))
            .count(),
        1
    );
    assert!(events.iter().any(|event| matches!(
        event,
        InteractiveSessionEvent::ActiveHandleChanged { active: false, .. }
    )));
}

#[tokio::test]
async fn close_is_idempotent_clears_queues_and_closes_event_subscriptions() {
    let session = InteractiveAgentClient::new(scripted_runner(vec![finish_response("unused")]))
        .create_session(
            scripted_agent(),
            InteractiveSessionOptions::new().session_id("close-idle"),
        )
        .await
        .expect("session");
    let mut events = session.subscribe();
    session.steer("queued steer").expect("steer");
    session.follow_up("queued follow-up").expect("follow-up");

    assert!(session.close());
    assert!(!session.close());
    assert!(session.closed());
    assert!(session.state().closed);
    assert_eq!(session.state().pending_steering, 0);
    assert_eq!(session.state().pending_follow_ups, 0);
    assert!(matches!(
        session.steer("late"),
        Err(InteractiveSessionError::Closed { .. })
    ));
    assert!(matches!(
        session.follow_up("late"),
        Err(InteractiveSessionError::Closed { .. })
    ));
    assert!(matches!(
        session.clear_queues(),
        Err(InteractiveSessionError::Closed { .. })
    ));
    assert!(matches!(
        session.prompt_once("late").await,
        Err(InteractiveSessionError::Closed { .. })
    ));

    let delivered = drain_events(&mut events);
    assert!(delivered.iter().any(|event| matches!(
        event,
        InteractiveSessionEvent::SessionClosed {
            session_id,
            aborted: false,
        } if session_id == "close-idle"
    )));
    assert!(matches!(
        events.try_recv(),
        Err(InteractiveSessionError::EventStreamClosed)
    ));
    let mut late_subscription = session.subscribe();
    assert!(matches!(
        late_subscription.recv().await,
        Err(InteractiveSessionError::EventStreamClosed)
    ));
}

#[tokio::test]
async fn close_cancels_the_active_run_and_reports_it_as_aborted() {
    let tool_started = Arc::new(tokio::sync::Notify::new());
    let release_tool = Arc::new(tokio::sync::Notify::new());
    let started = tool_started.clone();
    let release = release_tool.clone();
    let slow_tool = FunctionTool::builder("slow_tool")
        .description("Wait until the session closes.")
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
        .instructions("Use the slow tool.")
        .model(ModelRef::named("demo-model"))
        .tool(slow_tool)
        .build()
        .expect("agent");
    let session = InteractiveAgentClient::new(runner)
        .create_session(
            agent,
            InteractiveSessionOptions::new().session_id("close-running"),
        )
        .await
        .expect("session");
    let mut events = session.subscribe();
    let prompt_session = session.clone();
    let prompt_task = tokio::spawn(async move { prompt_session.prompt_once("initial").await });
    tokio::time::timeout(Duration::from_secs(5), tool_started.notified())
        .await
        .expect("tool start timeout");
    let stale_handle = session.active_run_handle().expect("active handle");

    assert!(session.close());
    assert!(session.closed());
    assert!(session.active_run_handle().is_none());
    assert_eq!(
        stale_handle.steer("late").expect_err("detached handle"),
        "RunHandle.steer() is only available when the handle is attached to an interactive session."
    );
    release_tool.notify_one();
    let prompt_result = tokio::time::timeout(Duration::from_secs(5), prompt_task)
        .await
        .expect("prompt timeout")
        .expect("prompt join");
    assert!(matches!(
        prompt_result,
        Err(InteractiveSessionError::Closed { .. })
    ));
    assert!(!session.running());

    let delivered = drain_events(&mut events);
    assert!(delivered.iter().any(|event| matches!(
        event,
        InteractiveSessionEvent::SessionClosed {
            session_id,
            aborted: true,
        } if session_id == "close-running"
    )));
    assert!(!delivered.iter().any(|event| matches!(
        event,
        InteractiveSessionEvent::RunFinished { .. } | InteractiveSessionEvent::RunAborted { .. }
    )));
}
