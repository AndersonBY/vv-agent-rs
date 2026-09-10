use super::*;

#[tokio::test]
async fn approval_resume_preserves_explicit_wait_and_finish_actions() {
    for (tool_name, arguments, expected_status, expected_reason, expected_output) in [
        (
            "ask_user",
            json!({"question": "Choose after approval"}),
            AgentStatus::WaitUser,
            CompletionReason::WaitUser,
            "Choose after approval",
        ),
        (
            "handoff_result",
            json!({"message": "finished after approval"}),
            AgentStatus::Completed,
            CompletionReason::ToolFinish,
            "finished after approval",
        ),
    ] {
        let provider = ScriptedModelProvider::new(
            "scripted",
            "approval-model",
            vec![LLMResponse::with_tool_calls(
                "assistant text before approved action",
                vec![ToolCall::from_raw_arguments(
                    "approved_action",
                    tool_name,
                    arguments,
                )],
            )],
        );
        let mut registry = vv_agent::build_default_registry();
        registry
            .register_tool_with_parameters(
                "handoff_result",
                "Return the delegated result.",
                json!({"type": "object", "properties": {"message": {"type": "string"}}, "required": ["message"], "additionalProperties": false}),
                Arc::new(|_context, arguments| {
                    let mut result = ToolExecutionResult::success(
                        "",
                        arguments["message"].as_str().expect("message"),
                    );
                    result.directive = ToolDirective::Finish;
                    result
                }),
            )
            .expect("result tool");
        let runner = Runner::builder()
            .tool_registry(registry)
            .model_provider(provider)
            .workspace("./workspace")
            .build()
            .expect("runner");
        let agent = Agent::builder("approval_agent")
            .instructions("Use the control tool.")
            .model(ModelRef::named("approval-model"))
            .tool_policy(approval_policy(ApprovalPolicy::Always))
            .build()
            .expect("agent");
        let interrupted = runner.run(&agent, "run").await.expect("interrupted");
        let interruption_id = interrupted.approvals()[0].interruption_id.clone();
        let mut state = interrupted.into_state().expect("state");
        state.approve(&interruption_id).expect("approve");

        let resumed = runner.resume(state).await.expect("resume");

        assert_eq!(resumed.status(), expected_status, "{tool_name}");
        assert_eq!(
            resumed.completion_reason(),
            Some(expected_reason),
            "{tool_name}"
        );
        assert_eq!(resumed.completion_tool_name(), Some(tool_name));
        assert_eq!(resumed.final_output(), Some(expected_output), "{tool_name}");
        if expected_status == AgentStatus::WaitUser {
            assert_eq!(
                resumed.partial_output(),
                Some("assistant text before approved action")
            );
        } else {
            assert_eq!(resumed.partial_output(), None);
        }
    }
}
