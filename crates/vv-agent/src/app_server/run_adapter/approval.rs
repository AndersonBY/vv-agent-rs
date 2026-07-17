use super::*;

pub(super) fn tool_approval_decision_from_response(
    value: Value,
) -> (ToolApprovalDecision, ApprovalDecision) {
    let action = value
        .get("decision")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let supplied_reason = value
        .get("reason")
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let metadata = value
        .get("metadata")
        .and_then(Value::as_object)
        .map(|metadata| {
            metadata
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<Metadata>()
        })
        .unwrap_or_default();
    let (decision, protocol_decision) = match action {
        "allow" => (ToolApprovalDecision::allow(), ApprovalDecision::Allow),
        "allow_session" => (
            ToolApprovalDecision::allow_session(),
            ApprovalDecision::AllowSession,
        ),
        "deny" => (
            ToolApprovalDecision::deny(if supplied_reason.is_empty() {
                "approval denied"
            } else {
                supplied_reason
            }),
            ApprovalDecision::Deny,
        ),
        "timeout" => (
            ToolApprovalDecision::timeout(if supplied_reason.is_empty() {
                "approval request timed out"
            } else {
                supplied_reason
            }),
            ApprovalDecision::Timeout,
        ),
        _ => (
            ToolApprovalDecision::deny("invalid approval response"),
            ApprovalDecision::Deny,
        ),
    };
    let decision = if supplied_reason.is_empty() {
        decision
    } else {
        decision.with_reason(supplied_reason)
    };
    let decision = if metadata.is_empty() {
        decision
    } else {
        decision.with_metadata(metadata)
    };
    (decision, protocol_decision)
}
