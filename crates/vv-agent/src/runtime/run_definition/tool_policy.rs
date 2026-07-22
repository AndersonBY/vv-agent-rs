use super::*;

pub(super) fn normalized_name_set(values: Option<&[String]>) -> Option<Vec<String>> {
    values.map(|values| {
        let mut values = values.to_vec();
        values.sort_by(|left, right| utf16_cmp(left, right));
        values.dedup();
        values
    })
}

pub(super) fn no_tool_policy_name(policy: NoToolPolicy) -> &'static str {
    match policy {
        NoToolPolicy::Continue => "continue",
        NoToolPolicy::WaitUser => "wait_user",
        NoToolPolicy::Finish => "finish",
    }
}

pub(super) fn approval_policy_name(policy: ApprovalPolicy) -> &'static str {
    match policy {
        ApprovalPolicy::Default => "default",
        ApprovalPolicy::Never => "never",
        ApprovalPolicy::Always => "always",
        ApprovalPolicy::OnRequest => "on_request",
    }
}

pub(super) fn tool_use_behavior_name(behavior: &ToolUseBehavior) -> &'static str {
    match behavior {
        ToolUseBehavior::RunLlmAgain => "run_llm_again",
        ToolUseBehavior::StopOnFirstTool => "stop_on_first_tool",
        ToolUseBehavior::StopAtToolNames(_) => "stop_at_tool_names",
    }
}

pub(super) fn stop_at_tool_names(behavior: &ToolUseBehavior) -> Vec<String> {
    match behavior {
        ToolUseBehavior::StopAtToolNames(names) => names.clone(),
        _ => Vec::new(),
    }
}

pub(super) fn utf16_cmp(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

pub(crate) fn tool_idempotency_for(registry: &ToolRegistry, name: &str) -> ToolIdempotency {
    registry
        .get(name)
        .ok()
        .and_then(|spec| spec.tool_metadata.as_ref())
        .map(|metadata| metadata.idempotency)
        .unwrap_or(ToolIdempotency::Unknown)
}
