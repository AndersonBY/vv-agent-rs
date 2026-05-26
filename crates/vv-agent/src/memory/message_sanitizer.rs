use std::collections::BTreeSet;

use crate::types::{Message, MessageRole, ToolCall};

pub fn sanitize_for_resume(messages: &[Message]) -> Vec<Message> {
    let sanitized = filter_empty_assistant_messages(messages);
    let sanitized = filter_thinking_only_messages(&sanitized);
    let sanitized = filter_orphan_tool_results(&sanitized);
    filter_unresolved_tool_uses(&sanitized)
}

pub fn filter_empty_assistant_messages(messages: &[Message]) -> Vec<Message> {
    messages
        .iter()
        .filter(|message| {
            message.role != MessageRole::Assistant
                || !message.content.trim().is_empty()
                || !message.tool_calls.is_empty()
                || has_thinking_content(message)
        })
        .cloned()
        .collect()
}

pub fn filter_thinking_only_messages(messages: &[Message]) -> Vec<Message> {
    messages
        .iter()
        .filter(|message| {
            message.role != MessageRole::Assistant
                || !has_thinking_content(message)
                || !message.content.trim().is_empty()
                || !message.tool_calls.is_empty()
        })
        .cloned()
        .collect()
}

pub fn filter_orphan_tool_results(messages: &[Message]) -> Vec<Message> {
    let call_ids = messages
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .flat_map(|message| message.tool_calls.iter())
        .map(|tool_call| tool_call.id.trim())
        .filter(|id| !id.is_empty())
        .collect::<BTreeSet<_>>();
    messages
        .iter()
        .filter(|message| {
            if message.role != MessageRole::Tool {
                return true;
            }
            let call_id = message.tool_call_id.as_deref().unwrap_or_default().trim();
            !call_id.is_empty() && call_ids.contains(call_id)
        })
        .cloned()
        .collect()
}

pub fn filter_unresolved_tool_uses(messages: &[Message]) -> Vec<Message> {
    let result_ids = messages
        .iter()
        .filter(|message| message.role == MessageRole::Tool)
        .filter_map(|message| message.tool_call_id.as_deref())
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .collect::<BTreeSet<_>>();

    let mut result = messages.to_vec();
    while !result.is_empty() {
        let Some(tail_index) = result
            .iter()
            .rposition(|message| message.role != MessageRole::Tool)
        else {
            break;
        };
        let last_message = &result[tail_index];
        if last_message.role != MessageRole::Assistant || last_message.tool_calls.is_empty() {
            break;
        }
        let unresolved_ids = last_message
            .tool_calls
            .iter()
            .map(|tool_call| tool_call.id.trim())
            .filter(|id| !id.is_empty() && !result_ids.contains(id))
            .collect::<BTreeSet<_>>();
        if unresolved_ids.is_empty() {
            break;
        }
        if unresolved_ids.len() == last_message.tool_calls.len() {
            result.remove(tail_index);
            continue;
        }

        let remaining_tool_calls = last_message
            .tool_calls
            .iter()
            .filter(|tool_call| !unresolved_ids.contains(tool_call.id.trim()))
            .cloned()
            .collect::<Vec<ToolCall>>();
        result[tail_index].tool_calls = remaining_tool_calls;
        break;
    }
    result
}

fn has_thinking_content(message: &Message) -> bool {
    message
        .reasoning_content
        .as_deref()
        .is_some_and(|text| !text.trim().is_empty())
}
