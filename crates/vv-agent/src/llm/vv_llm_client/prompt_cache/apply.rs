use serde_json::Value;

use super::from_cache::cache_content_to_vv_llm_content;

pub(super) fn apply_planned_message_content(
    messages: &mut [vv_llm::Message],
    planned_messages: Vec<Value>,
) {
    for (message, planned_message) in messages.iter_mut().zip(planned_messages) {
        let Some(content) = planned_message.get("content") else {
            continue;
        };
        message.content = cache_content_to_vv_llm_content(content);
    }
}

pub(super) fn apply_planned_tool_cache_control(
    tools: &mut [vv_llm::ChatTool],
    planned_tools: Vec<Value>,
) {
    for (tool, planned_tool) in tools.iter_mut().zip(planned_tools) {
        if let Some(cache_control) = planned_tool.get("cache_control") {
            tool.cache_control = Some(cache_control.clone());
        }
    }
}
