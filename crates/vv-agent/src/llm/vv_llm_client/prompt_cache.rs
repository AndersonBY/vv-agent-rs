use serde_json::Value;

use crate::llm::anthropic_prompt_cache::apply_claude_prompt_cache;

mod apply;
mod endpoint;
mod from_cache;
mod metadata;
mod to_cache;

pub(super) use endpoint::endpoint_type_for_prompt_cache;
pub(super) use metadata::request_metadata_for_prompt_cache;

use apply::{apply_planned_message_content, apply_planned_tool_cache_control};
use to_cache::{vv_llm_message_to_cache_json, vv_llm_tool_to_cache_json};

pub(super) fn apply_prompt_cache_to_chat_request(
    endpoint_type: &str,
    model: &str,
    metadata: &Value,
    chat_request: &mut vv_llm::ChatRequest,
) {
    let messages = chat_request
        .messages
        .iter()
        .map(vv_llm_message_to_cache_json)
        .collect::<Vec<_>>();
    let tools = chat_request
        .tools
        .iter()
        .map(vv_llm_tool_to_cache_json)
        .collect::<Vec<_>>();
    let (planned_messages, planned_tools, planned_extra_body) = apply_claude_prompt_cache(
        endpoint_type,
        model,
        &messages,
        &tools,
        Some(&chat_request.extra_body),
        Some(metadata),
    );
    apply_planned_message_content(&mut chat_request.messages, planned_messages);
    apply_planned_tool_cache_control(&mut chat_request.tools, planned_tools);
    if let Some(extra_body) = planned_extra_body {
        chat_request.extra_body = extra_body;
    }
}
