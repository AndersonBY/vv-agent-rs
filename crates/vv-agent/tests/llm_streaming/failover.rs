use super::*;

#[test]
fn vv_llm_client_fails_over_to_next_endpoint_client() {
    let llm = VvLlmClient::new_with_named_endpoint_clients(
        "openai",
        "gpt-4o-alias",
        "gpt-4o-mini",
        vec![
            (
                "primary-endpoint".to_string(),
                "gpt-4o-mini".to_string(),
                Box::new(FailingChatClient) as Box<dyn vv_llm::ChatClient>,
            ),
            (
                "backup-endpoint".to_string(),
                "gpt-4o-mini-backup".to_string(),
                Box::new(UsageMissingChatClient) as Box<dyn vv_llm::ChatClient>,
            ),
        ],
        90.0,
    )
    .with_randomize_endpoints(false)
    .with_retry_policy(1, 0.0);

    let response = llm
        .complete(LlmRequest::new(
            "gpt-4o-alias",
            vec![Message::user("fall over to backup endpoint")],
        ))
        .expect("completion from fallback endpoint");

    assert_eq!(response.content, "estimated usage response");
    assert_eq!(response.raw["used_endpoint_id"], json!("backup-endpoint"));
    assert_eq!(response.raw["used_model_id"], json!("gpt-4o-mini-backup"));
    assert_eq!(response.raw["stream_mode"], json!(true));
}

#[test]
fn vv_llm_client_prefers_last_successful_endpoint() {
    let primary = CountingFailingChatClient::default();
    let primary_probe = primary.clone();
    let llm = VvLlmClient::new_with_named_endpoint_clients(
        "openai",
        "gpt-4o-mini",
        "gpt-4o-mini",
        vec![
            (
                "primary-endpoint".to_string(),
                "gpt-4o-mini".to_string(),
                Box::new(primary) as Box<dyn vv_llm::ChatClient>,
            ),
            (
                "backup-endpoint".to_string(),
                "gpt-4o-mini".to_string(),
                Box::new(UsageMissingChatClient) as Box<dyn vv_llm::ChatClient>,
            ),
        ],
        90.0,
    )
    .with_randomize_endpoints(false)
    .with_retry_policy(1, 0.0);

    let first = llm
        .complete(LlmRequest::new("gpt-4o-mini", vec![Message::user("first")]))
        .expect("first fallback completion");
    let second = llm
        .complete(LlmRequest::new(
            "gpt-4o-mini",
            vec![Message::user("second")],
        ))
        .expect("second preferred completion");

    assert_eq!(first.raw["used_endpoint_id"], json!("backup-endpoint"));
    assert_eq!(second.raw["used_endpoint_id"], json!("backup-endpoint"));
    assert_eq!(primary_probe.calls(), 1);
}

#[test]
fn vv_llm_client_exposes_agent_endpoint_randomization_policy() {
    let llm = VvLlmClient::new_with_named_endpoint_clients(
        "openai",
        "gpt-4o-mini",
        "gpt-4o-mini",
        vec![
            (
                "primary-endpoint".to_string(),
                "gpt-4o-mini".to_string(),
                Box::new(UsageMissingChatClient) as Box<dyn vv_llm::ChatClient>,
            ),
            (
                "backup-endpoint".to_string(),
                "gpt-4o-mini".to_string(),
                Box::new(UsageMissingChatClient) as Box<dyn vv_llm::ChatClient>,
            ),
        ],
        90.0,
    );

    assert!(llm.randomize_endpoints());
    let llm = llm.with_randomize_endpoints(false);
    assert!(!llm.randomize_endpoints());
}

#[test]
fn vv_llm_client_retries_endpoint_before_failover() {
    let flaky = FlakyChatClient::new(1);
    let flaky_probe = flaky.clone();
    let llm = VvLlmClient::new(
        "openai",
        "gpt-4o-mini",
        "gpt-4o-mini",
        Box::new(flaky),
        90.0,
    )
    .with_retry_policy(2, 0.0);

    let response = llm
        .complete(LlmRequest::new(
            "gpt-4o-mini",
            vec![Message::user("retry once")],
        ))
        .expect("retry succeeds");

    assert_eq!(response.content, "flaky success");
    assert_eq!(flaky_probe.calls(), 2);
}

#[test]
fn vv_llm_client_uses_endpoint_model_for_selected_alias() {
    let llm = VvLlmClient::new_with_named_endpoint_clients(
        "openai",
        "gpt-alias",
        "gpt-provider-model",
        vec![(
            "primary-endpoint".to_string(),
            "gpt-provider-model".to_string(),
            Box::new(UsageMissingChatClient) as Box<dyn vv_llm::ChatClient>,
        )],
        90.0,
    );

    let response = llm
        .complete(LlmRequest::new(
            "gpt-alias",
            vec![Message::user("use provider model id")],
        ))
        .expect("completion from primary endpoint");

    assert_eq!(response.raw["used_model_id"], json!("gpt-provider-model"));
}
