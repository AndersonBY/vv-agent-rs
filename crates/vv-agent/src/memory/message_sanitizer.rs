use std::collections::BTreeMap;

use crate::types::{Message, MessageRole, ToolCall};

pub fn sanitize_for_resume(messages: &[Message]) -> Vec<Message> {
    let sanitized = filter_empty_assistant_messages(messages);
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
    filter_tool_turns(messages, true, false)
}

pub fn filter_unresolved_tool_uses(messages: &[Message]) -> Vec<Message> {
    filter_tool_turns(messages, false, true)
}

fn filter_tool_turns(
    messages: &[Message],
    drop_orphan_results: bool,
    drop_unresolved_calls: bool,
) -> Vec<Message> {
    let mut result = Vec::new();
    let mut index = 0;
    while index < messages.len() {
        let message = &messages[index];
        if message.role == MessageRole::Tool {
            if !drop_orphan_results {
                result.push(message.clone());
            }
            index += 1;
            continue;
        }
        if message.role != MessageRole::Assistant || message.tool_calls.is_empty() {
            result.push(message.clone());
            index += 1;
            continue;
        }

        let mut result_end = index + 1;
        while result_end < messages.len() && messages[result_end].role == MessageRole::Tool {
            result_end += 1;
        }
        let tool_results = &messages[index + 1..result_end];

        let mut call_counts = BTreeMap::<String, usize>::new();
        for tool_call in &message.tool_calls {
            let call_id = tool_call.id.trim();
            if !call_id.is_empty() {
                *call_counts.entry(call_id.to_string()).or_default() += 1;
            }
        }
        let mut result_counts = BTreeMap::<String, usize>::new();
        for tool_result in tool_results {
            let call_id = tool_result
                .tool_call_id
                .as_deref()
                .unwrap_or_default()
                .trim();
            if !call_id.is_empty() {
                *result_counts.entry(call_id.to_string()).or_default() += 1;
            }
        }
        let ordered_calls = message
            .tool_calls
            .iter()
            .filter_map(|tool_call| {
                let call_id = tool_call.id.trim();
                (!call_id.is_empty()).then_some((call_id, tool_call))
            })
            .collect::<Vec<_>>();
        let ordered_results = tool_results
            .iter()
            .filter_map(|tool_result| {
                let call_id = tool_result
                    .tool_call_id
                    .as_deref()
                    .unwrap_or_default()
                    .trim();
                (!call_id.is_empty()).then_some((call_id, tool_result))
            })
            .collect::<Vec<_>>();
        let mut paired_calls = Vec::<ToolCall>::new();
        let mut paired_results = Vec::<Message>::new();
        for ((call_id, tool_call), (result_id, tool_result)) in
            ordered_calls.into_iter().zip(ordered_results)
        {
            if call_id != result_id
                || call_counts.get(call_id) != Some(&1)
                || result_counts.get(result_id) != Some(&1)
            {
                break;
            }
            paired_calls.push(tool_call.clone());
            paired_results.push(tool_result.clone());
        }
        let visible_calls = if drop_unresolved_calls {
            paired_calls.clone()
        } else {
            message.tool_calls.clone()
        };
        if !visible_calls.is_empty() {
            let mut paired_assistant = message.clone();
            paired_assistant.tool_calls = visible_calls;
            result.push(paired_assistant);
        }
        if !drop_orphan_results {
            result.extend_from_slice(tool_results);
        } else if !paired_calls.is_empty() {
            result.extend(paired_results);
        }
        index = result_end;
    }
    result
}

fn has_thinking_content(message: &Message) -> bool {
    message
        .reasoning_content
        .as_deref()
        .is_some_and(|text| !text.trim().is_empty())
}
