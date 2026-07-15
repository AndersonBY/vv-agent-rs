use super::*;

#[test]
fn result_preserves_completion_without_error_code() {
    let session = runtime_session(None);
    let result = AgentResult {
        status: AgentStatus::WaitUser,
        completion_reason: Some(CompletionReason::WaitUser),
        completion_tool_name: Some("dangerous".to_string()),
        partial_output: Some("proposed change".to_string()),
        wait_reason: Some("Approve dangerous.".to_string()),
        ..AgentResult::default()
    };

    let outcome = session.outcome_from_result(result);

    assert_eq!(outcome.status, AgentStatus::WaitUser);
    assert_eq!(outcome.error_code, None);
    assert_eq!(outcome.completion_reason, Some(CompletionReason::WaitUser));
    assert_eq!(outcome.completion_tool_name.as_deref(), Some("dangerous"));
    assert_eq!(outcome.partial_output.as_deref(), Some("proposed change"));
}

#[test]
fn sub_runtime_inherits_settings_file_and_default_backend() {
    let settings_file = PathBuf::from("/contract/llm-settings.json");
    let mut session = runtime_session(None);
    session.settings_file = Some(settings_file.clone());
    session.default_backend = Some("contract-backend".to_string());

    let runtime = session.build_runtime(&session.tool_policy);

    assert_eq!(
        runtime.settings_file.as_deref(),
        Some(settings_file.as_path())
    );
    assert_eq!(runtime.default_backend.as_deref(), Some("contract-backend"));
}
