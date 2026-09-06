use super::*;

#[tokio::test]
async fn checkpointed_approval_resume_uses_distinct_target_and_replays_idempotently() {
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
    let source_key = "approval-checkpoint-source";
    let target_key = "approval-checkpoint-target";
    let mut checkpoint = CheckpointConfig::with_store(store.clone());
    checkpoint.key = Some(source_key.to_string());
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
        .load_checkpoint(source_key)
        .expect("load source checkpoint")
        .expect("source checkpoint");
    assert!(before.tool_journal.is_empty());
    let interruption_id = interrupted.approvals()[0].interruption_id.clone();
    let mut state = interrupted.into_state().expect("state");
    state.approve(&interruption_id).expect("approve");
    let retry_state = state.clone();

    let mut target_checkpoint = CheckpointConfig::with_store(store.clone());
    target_checkpoint.key = Some(target_key.to_string());
    target_checkpoint.resume_policy = ResumePolicy::ResumeIfPresent;
    target_checkpoint.capability_refs.insert(
        "session".to_string(),
        CapabilityRef::new("session.runner-checkpoint", "1").expect("capability ref"),
    );
    let target_runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "approval-model",
            vec![],
        ))
        .workspace("./workspace")
        .default_run_config(
            RunConfig::builder()
                .checkpoint_config(target_checkpoint)
                .build(),
        )
        .build()
        .expect("target runner");

    let resumed = target_runner.resume(state).await.expect("target resume");
    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    let target = store
        .load_checkpoint(target_key)
        .expect("load target checkpoint")
        .expect("target checkpoint");
    assert!(target.terminal_result.is_some());
    assert!(target.tool_journal.iter().all(|entry| {
        entry.state == vv_agent::OperationState::Succeeded
            && entry.tool_call_id.as_deref() == Some("tool_call")
    }));
    let session_before_replay = session.get_items(None).await.expect("session items");
    let replay = target_runner
        .resume(retry_state.clone())
        .await
        .expect("same target replay");
    assert_eq!(replay.run_id(), resumed.run_id());
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    let session_after_replay = session
        .get_items(None)
        .await
        .expect("replayed session items");
    assert_eq!(session_after_replay, session_before_replay);

    let mut other_checkpoint = CheckpointConfig::with_store(store.clone());
    other_checkpoint.key = Some("approval-checkpoint-other".to_string());
    other_checkpoint.resume_policy = ResumePolicy::ResumeIfPresent;
    let other_runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "approval-model",
            vec![],
        ))
        .workspace("./workspace")
        .default_run_config(
            RunConfig::builder()
                .checkpoint_config(other_checkpoint)
                .build(),
        )
        .build()
        .expect("other runner");
    let error = match other_runner.resume(retry_state).await {
        Ok(_) => panic!("different target must reject"),
        Err(error) => error,
    };
    assert_eq!(error, "approval_already_consumed");
    assert_eq!(
        store
            .load_checkpoint(source_key)
            .expect("reload source checkpoint")
            .expect("source checkpoint after resume"),
        before
    );
}

#[tokio::test]
async fn checkpointed_approval_resume_continue_reenters_target_loop_without_model_for_tool() {
    let executions = Arc::new(AtomicUsize::new(0));
    let executions_for_tool = executions.clone();
    let tool = FunctionTool::builder("guarded_checkpoint_continue")
        .needs_approval(true)
        .handler(move |_context, _arguments: Value| {
            let executions = executions_for_tool.clone();
            async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text("approved result"))
            }
        })
        .build()
        .expect("guarded tool");
    let (runner, agent) = runner_and_agent(
        tool,
        ApprovalPolicy::OnRequest,
        vec![single_tool_response("guarded_checkpoint_continue")],
        ToolUseBehavior::RunLlmAgain,
    );
    let session = MemorySession::new("approval-checkpoint-continue-session");
    let store = InMemoryCheckpointStore::new();
    let mut source_checkpoint = CheckpointConfig::with_store(store.clone());
    source_checkpoint.key = Some("approval-checkpoint-continue-source".to_string());
    source_checkpoint.capability_refs.insert(
        "session".to_string(),
        CapabilityRef::new("session.runner-checkpoint", "1").expect("capability ref"),
    );
    let interrupted = runner
        .run_with_config(
            &agent,
            "continue after approval",
            RunConfig::builder()
                .session(session.clone())
                .checkpoint_config(source_checkpoint)
                .build(),
        )
        .await
        .expect("checkpointed approval interruption");
    let interruption_id = interrupted.approvals()[0].interruption_id.clone();
    let mut state = interrupted.into_state().expect("state");
    state.approve(&interruption_id).expect("approve");

    let mut target_checkpoint = CheckpointConfig::with_store(store.clone());
    target_checkpoint.key = Some("approval-checkpoint-continue-target".to_string());
    target_checkpoint.resume_policy = ResumePolicy::ResumeIfPresent;
    target_checkpoint.capability_refs.insert(
        "session".to_string(),
        CapabilityRef::new("session.runner-checkpoint", "1").expect("capability ref"),
    );
    let target_runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "approval-model",
            vec![finish_response("continued")],
        ))
        .workspace("./workspace")
        .default_run_config(
            RunConfig::builder()
                .checkpoint_config(target_checkpoint)
                .build(),
        )
        .build()
        .expect("target runner");

    let resumed = target_runner.resume(state).await.expect("target resume");

    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_eq!(resumed.final_output(), Some("continued"));
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    let target = store
        .load_checkpoint("approval-checkpoint-continue-target")
        .expect("load target checkpoint")
        .expect("target checkpoint");
    assert!(target.terminal_result.is_some());
    assert_eq!(target.model_calls.len(), 1);
    let terminal_result = target.terminal_result.as_ref().expect("terminal result");
    let terminal_messages = terminal_result
        .get("messages")
        .and_then(Value::as_array)
        .expect("terminal messages");
    assert!(terminal_messages.iter().any(|message| {
        message.get("role") == Some(&Value::String("tool".to_string()))
            && message.get("tool_call_id") == Some(&Value::String("tool_call".to_string()))
            && message
                .get("content")
                .and_then(Value::as_str)
                .is_some_and(|content| content.contains("approved result"))
    }));
}
