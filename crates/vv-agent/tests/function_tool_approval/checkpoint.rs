use super::*;

#[tokio::test]
async fn checkpointed_approval_resume_rejects_before_handler_or_session_write() {
    let executions = Arc::new(AtomicUsize::new(0));
    let executions_for_tool = executions.clone();
    let tool = FunctionTool::builder("guarded_checkpoint")
        .needs_approval(true)
        .handler(move |_context, _arguments: Value| {
            let executions = executions_for_tool.clone();
            async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text("executed"))
            }
        })
        .build()
        .expect("guarded tool");
    let (runner, agent) = runner_and_agent(
        tool,
        ApprovalPolicy::OnRequest,
        vec![single_tool_response("guarded_checkpoint")],
        ToolUseBehavior::StopOnFirstTool,
    );
    let session = MemorySession::new("approval-checkpoint-session");
    let store = InMemoryCheckpointStore::new();
    let checkpoint_key = "approval-checkpoint-rejected";
    let mut checkpoint = CheckpointConfig::with_store(store.clone());
    checkpoint.key = Some(checkpoint_key.to_string());
    checkpoint.capability_refs.insert(
        "session".to_string(),
        CapabilityRef::new("session.runner-checkpoint", "1").expect("capability ref"),
    );

    let interrupted = runner
        .run_with_config(
            &agent,
            "run once",
            RunConfig::builder()
                .session(session.clone())
                .checkpoint_config(checkpoint)
                .build(),
        )
        .await
        .expect("checkpointed approval interruption");
    assert_eq!(interrupted.status(), AgentStatus::WaitUser);
    let before = store
        .load_checkpoint(checkpoint_key)
        .expect("load checkpoint before rejected resume")
        .expect("checkpoint before rejected resume");
    let session_before = session
        .get_items(None)
        .await
        .expect("session before resume");
    let interruption_id = interrupted.approvals()[0].interruption_id.clone();
    let mut state = interrupted.into_state().expect("state");
    state.approve(&interruption_id).expect("approve");

    let error = match runner.resume(state).await {
        Ok(_) => panic!("checkpoint approval must fail closed"),
        Err(error) => error,
    };
    assert_eq!(
        error,
        "checkpoint_approval_resume_config_invalid: checkpoint approval resume requires a distinct explicit resume_if_present key"
    );
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert_eq!(
        session.get_items(None).await.expect("session after resume"),
        session_before
    );
    assert_eq!(
        store
            .load_checkpoint(checkpoint_key)
            .expect("load checkpoint after rejected resume")
            .expect("checkpoint after rejected resume"),
        before
    );
}
