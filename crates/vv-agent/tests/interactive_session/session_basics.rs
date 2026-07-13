#[tokio::test]
async fn session_preserves_id_hydrates_messages_shared_state_and_events() {
    let storage = MemorySession::new("desktop-session-1");
    storage
        .add_items(vec![
            SessionItem::User {
                content: "earlier question".to_string(),
            },
            SessionItem::Assistant {
                content: "earlier answer".to_string(),
            },
        ])
        .await
        .expect("seed session");
    let runner = scripted_runner(vec![finish_response("current answer")]);
    let agent = scripted_agent();
    let mut shared_state = BTreeMap::new();
    shared_state.insert("host".to_string(), json!("desktop"));
    let session = InteractiveAgentClient::new(runner)
        .create_session(
            agent,
            InteractiveSessionOptions::new()
                .session_id("desktop-session-1")
                .session(storage.clone())
                .shared_state(shared_state),
        )
        .await
        .expect("create session");
    let mut events = session.subscribe();

    assert_eq!(session.session_id(), "desktop-session-1");
    assert_eq!(session.messages().len(), 2);

    let result = session.prompt("current question").await.expect("prompt");

    assert_eq!(result.final_output(), Some("current answer"));
    assert_eq!(result.result().shared_state["host"], "desktop");
    assert!(session
        .messages()
        .iter()
        .any(|message| message.content == "current question"));
    assert_eq!(session.shared_state()["host"], "desktop");
    assert_eq!(session.shared_state()["todo_list"], json!([]));
    assert_eq!(
        session
            .latest_run()
            .and_then(|run| run.final_output().map(str::to_string))
            .as_deref(),
        Some("current answer")
    );
    assert!(!session.running());
    assert!(session.active_run_handle().is_none());

    let events = drain_events(&mut events);
    assert!(matches!(
        events.first(),
        Some(InteractiveSessionEvent::RunStarted { session_id, .. })
            if session_id == "desktop-session-1"
    ));
    assert!(events.iter().any(|event| matches!(
        event,
        InteractiveSessionEvent::RunEvent { session_id, event }
            if session_id == "desktop-session-1"
                && event.session_id() == Some("desktop-session-1")
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        InteractiveSessionEvent::RunFinished {
            session_id,
            status: AgentStatus::Completed,
            ..
        } if session_id == "desktop-session-1"
    )));

    let stored = storage.get_items(None).await.expect("stored messages");
    let stored_messages = stored
        .iter()
        .map(SessionItem::to_message)
        .collect::<Vec<_>>();
    assert_eq!(session.messages(), stored_messages);
    assert!(session
        .messages()
        .first()
        .is_none_or(|message| message.role != vv_agent::MessageRole::System));
    assert!(stored.iter().any(
        |item| matches!(item, SessionItem::User { content } if content == "current question")
    ));
    assert!(stored
        .iter()
        .any(|item| item.to_message().content.contains("current answer")));
}

#[tokio::test]
async fn run_config_session_is_used_when_options_session_is_absent() {
    let storage = MemorySession::new("run-config-session");
    storage
        .add_items(vec![SessionItem::User {
            content: "stored".to_string(),
        }])
        .await
        .expect("seed session");
    let session = InteractiveAgentClient::new(scripted_runner(vec![finish_response("done")]))
        .create_session(
            scripted_agent(),
            InteractiveSessionOptions::new().run_config(
                vv_agent::RunConfig::builder()
                    .session(storage.clone())
                    .build(),
            ),
        )
        .await
        .expect("create session");

    assert_eq!(session.session_id(), "run-config-session");
    assert_eq!(session.messages(), vec![Message::user("stored")]);
}

#[tokio::test(flavor = "multi_thread")]
async fn allow_session_persists_across_automatic_follow_up() {
    let executions = Arc::new(AtomicUsize::new(0));
    let tool_executions = executions.clone();
    let dangerous = FunctionTool::builder("dangerous")
        .description("Run an approved operation.")
        .json_schema(json!({"type": "object", "properties": {}, "required": []}))
        .needs_approval(true)
        .handler(move |_context, _arguments: Value| {
            tool_executions.fetch_add(1, Ordering::SeqCst);
            async { Ok(ToolOutput::text("allowed")) }
        })
        .build()
        .expect("dangerous tool");
    let provider = ScriptedModelProvider::new(
        "scripted",
        "demo-model",
        vec![
            LLMResponse::with_tool_calls(
                "first call",
                vec![ToolCall::new("call_1", "dangerous", BTreeMap::new())],
            ),
            finish_response("first"),
            LLMResponse::with_tool_calls(
                "second call",
                vec![ToolCall::new("call_2", "dangerous", BTreeMap::new())],
            ),
            finish_response("second"),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Run the operation and finish.")
        .model(ModelRef::named("demo-model"))
        .tool(dangerous)
        .build()
        .expect("agent");
    let approval_requests = Arc::new(AtomicUsize::new(0));
    let (request_tx, request_rx) = std::sync::mpsc::channel();
    let session = InteractiveAgentClient::new(runner)
        .create_session(
            agent,
            InteractiveSessionOptions::new()
                .session_id("allow-session")
                .run_config(
                    vv_agent::RunConfig::builder()
                        .approval_provider(Arc::new(DeferredApprovalProvider {
                            requests: approval_requests.clone(),
                            request_ids: request_tx,
                        }))
                        .approval_timeout(Duration::from_secs(5))
                        .build(),
                ),
        )
        .await
        .expect("session");
    session.follow_up("run it again").expect("follow-up");
    let prompt_session = session.clone();
    let prompt_task = tokio::spawn(async move { prompt_session.prompt("run it").await });

    let request_id = request_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("approval request");
    session
        .approve(request_id, ApprovalDecision::allow_session())
        .expect("approve for session");

    let result = prompt_task
        .await
        .expect("prompt task")
        .expect("prompt result");
    assert_eq!(result.final_output(), Some("second"));
    assert_eq!(executions.load(Ordering::SeqCst), 2);
    assert_eq!(approval_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_can_resolve_request_synchronously_from_decide() {
    let executions = Arc::new(AtomicUsize::new(0));
    let tool_executions = executions.clone();
    let dangerous = FunctionTool::builder("dangerous")
        .description("Run an approved operation.")
        .json_schema(json!({"type": "object", "properties": {}, "required": []}))
        .needs_approval(true)
        .handler(move |_context, _arguments: Value| {
            tool_executions.fetch_add(1, Ordering::SeqCst);
            async { Ok(ToolOutput::text("allowed")) }
        })
        .build()
        .expect("dangerous tool");
    let provider = ScriptedModelProvider::new(
        "scripted",
        "demo-model",
        vec![
            LLMResponse::with_tool_calls(
                "call",
                vec![ToolCall::new("call_1", "dangerous", BTreeMap::new())],
            ),
            finish_response("done"),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Run the operation and finish.")
        .model(ModelRef::named("demo-model"))
        .tool(dangerous)
        .build()
        .expect("agent");
    let broker = ApprovalBroker::default();
    let session = InteractiveAgentClient::new(runner)
        .create_session(
            agent,
            InteractiveSessionOptions::new()
                .session_id("synchronous-approval")
                .run_config(
                    vv_agent::RunConfig::builder()
                        .approval_broker(broker.clone())
                        .approval_provider(Arc::new(SynchronouslyResolvedApprovalProvider {
                            broker,
                        }))
                        .approval_timeout(Duration::from_secs(5))
                        .build(),
                ),
        )
        .await
        .expect("session");

    let result = session.prompt("run it").await.expect("prompt result");

    assert_eq!(result.final_output(), Some("done"));
    assert_eq!(executions.load(Ordering::SeqCst), 1);
}
