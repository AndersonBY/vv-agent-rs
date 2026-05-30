use super::*;

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

#[test]
fn session_continue_after_wait_user_with_multiple_tool_calls() {
    let responses = vec![
        LLMResponse {
            content: "need user input".to_string(),
            tool_calls: vec![
                ToolCall::new(
                    "u1",
                    "ask_user",
                    json_args(serde_json::json!({"question": "pick style"})),
                ),
                ToolCall::new(
                    "u2",
                    "ask_user",
                    json_args(serde_json::json!({"question": "pick output file"})),
                ),
            ],
            raw: BTreeMap::new(),
            token_usage: TokenUsage::default(),
        },
        LLMResponse {
            content: "finish".to_string(),
            tool_calls: vec![ToolCall::new(
                "u3",
                "task_finish",
                json_args(serde_json::json!({"message": "done"})),
            )],
            raw: BTreeMap::new(),
            token_usage: TokenUsage::default(),
        },
    ];
    let mut client = AgentSDKClient::new(AgentSDKOptions::default())
        .with_runtime(AgentRuntime::new(ScriptedLlmClient::new(responses)));
    client.set_default_agent(AgentDefinition::default_for_model("demo"));
    let mut session = client.create_default_session().expect("default session");

    let first = session
        .prompt_with_auto_follow_up("start", false)
        .expect("first prompt");

    assert_eq!(first.result.status, AgentStatus::WaitUser);
    assert_eq!(first.result.cycles[0].tool_results.len(), 2);
    assert_eq!(
        first.result.cycles[0].tool_results[1].error_code.as_deref(),
        Some("skipped_due_to_wait_user")
    );

    let second = session
        .continue_run(Some(
            "formal style, write to artifacts/result.md".to_string(),
        ))
        .expect("continue run");

    assert_eq!(second.result.status, AgentStatus::Completed);
    assert_eq!(second.result.final_answer.as_deref(), Some("done"));
}
