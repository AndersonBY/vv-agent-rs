use super::*;

#[test]
fn cancellation_token_propagates_to_children_and_runtime() {
    let parent = CancellationToken::default();
    let child = parent.child();
    assert!(!parent.is_cancelled());
    assert!(!child.is_cancelled());

    parent.cancel();

    assert!(parent.is_cancelled());
    assert!(child.is_cancelled());

    let runtime = AgentRuntime::new(ScriptedLlmClient::new(vec![LLMResponse::new(
        "should not be used",
    )]));
    let result = runtime
        .run_with_controls(
            AgentTask::new("cancel_task", "demo", "system", "start"),
            RuntimeRunControls {
                cancellation_token: Some(parent),
                ..RuntimeRunControls::default()
            },
        )
        .expect("cancelled result");

    assert_eq!(result.status, AgentStatus::Failed);
    assert!(result
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("cancelled"));
    assert!(result.cycles.is_empty());
}

#[test]
fn cancellation_token_callbacks_match_agent_semantics() {
    let token = CancellationToken::default();
    assert!(!token.cancelled());
    assert!(token.check().is_ok());

    let calls = Arc::new(Mutex::new(Vec::new()));
    let callback_calls = Arc::clone(&calls);
    token.on_cancel(move || {
        callback_calls
            .lock()
            .expect("callback calls lock")
            .push("first");
    });
    assert!(calls.lock().expect("callback calls lock").is_empty());

    token.cancel();
    token.cancel();

    assert!(token.cancelled());
    assert_eq!(*calls.lock().expect("callback calls lock"), vec!["first"]);
    let error: vv_agent::CancelledError = token.check().expect_err("typed cancellation");
    assert_eq!(error.message(), "Operation was cancelled");
    let context_error: vv_agent::CancelledError = ExecutionContext {
        cancellation_token: Some(token.clone()),
        ..ExecutionContext::default()
    }
    .check_cancelled()
    .expect_err("typed context cancellation");
    assert_eq!(context_error, error);

    let immediate_calls = Arc::new(Mutex::new(Vec::new()));
    let callback_calls = Arc::clone(&immediate_calls);
    token.on_cancel(move || {
        callback_calls
            .lock()
            .expect("immediate callback calls lock")
            .push("immediate");
    });
    assert_eq!(
        *immediate_calls
            .lock()
            .expect("immediate callback calls lock"),
        vec!["immediate"]
    );

    let parent = CancellationToken::default();
    let child = parent.child();
    let grandchild = child.child();
    child.cancel();
    assert!(child.cancelled());
    assert!(child.is_cancelled());
    assert!(grandchild.is_cancelled());
    assert!(!parent.cancelled());
    assert!(!parent.is_cancelled());
}

#[test]
fn execution_context_cancellation_token_is_honored_by_runtime() {
    let token = CancellationToken::default();
    token.cancel();
    let context = vv_agent::ExecutionContext::default().with_cancellation_token(token);
    let runtime = AgentRuntime::new(ScriptedLlmClient::new(vec![LLMResponse::new(
        "should not be used",
    )]));

    let result = runtime
        .run_with_controls(
            AgentTask::new("ctx_cancel_task", "demo", "system", "start"),
            RuntimeRunControls {
                execution_context: Some(context),
                ..RuntimeRunControls::default()
            },
        )
        .expect("cancelled result");

    assert_eq!(result.status, AgentStatus::Failed);
    assert!(result
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("cancelled"));
    assert!(result.cycles.is_empty());
}

#[test]
fn cancellation_token_cancelled_by_before_cycle_provider_stops_before_llm() {
    let second_llm_calls = Arc::new(Mutex::new(0_u32));
    let calls = Arc::clone(&second_llm_calls);
    let llm = ScriptedLlmClient::from_steps(vec![
        LLMResponse::new("cycle1").into(),
        vv_agent::ScriptStep::callback(move |_request| {
            *calls.lock().expect("second llm calls") += 1;
            Ok(LLMResponse::new("cycle2 should not run"))
        }),
    ]);
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new("cancel_between_cycles", "demo", "system", "start");
    task.max_cycles = 3;
    task.no_tool_policy = vv_agent::NoToolPolicy::Continue;
    let token = CancellationToken::default();
    let token_for_provider = token.clone();

    let result = runtime
        .run_with_controls(
            task,
            RuntimeRunControls {
                cancellation_token: Some(token),
                before_cycle_messages: Some(Arc::new(move |cycle_index, _messages, _state| {
                    if cycle_index == 2 {
                        token_for_provider.cancel();
                    }
                    Vec::new()
                })),
                ..RuntimeRunControls::default()
            },
        )
        .expect("cancelled result");

    assert_eq!(result.status, AgentStatus::Failed);
    assert!(result
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("cancelled"));
    assert_eq!(result.cycles.len(), 1);
    assert_eq!(*second_llm_calls.lock().expect("second llm calls"), 0);
}
