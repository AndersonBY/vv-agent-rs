use serde_json::{json, Map, Value};

use super::blocks::{
    block_type, content_blocks, ensure_content_blocks, is_thinking_block_type, set_cache_control,
};
use super::estimate::{estimate_block_chars, estimate_tokens, estimate_tool_chars};
use super::sections::normalize_system_prompt_sections;
use super::{cache_control_ephemeral, SYSTEM_PROMPT_SECTIONS_KEY};

const MAX_BREAKPOINTS: usize = 4;

pub(super) fn apply_cache_breakpoints(
    messages: &mut [Value],
    tools: &mut [Value],
    metadata: Option<&Map<String, Value>>,
    token_threshold: usize,
) {
    let mut breakpoint_budget = MAX_BREAKPOINTS;
    let system_char_count =
        apply_system_cache_breakpoint(messages, metadata, token_threshold, &mut breakpoint_budget);
    let tool_char_count = apply_tool_cache_breakpoint(
        tools,
        system_char_count,
        token_threshold,
        &mut breakpoint_budget,
    );
    apply_history_cache_breakpoint(
        messages,
        system_char_count + tool_char_count,
        token_threshold,
        breakpoint_budget,
    );
}

fn apply_system_cache_breakpoint(
    messages: &mut [Value],
    metadata: Option<&Map<String, Value>>,
    token_threshold: usize,
    breakpoint_budget: &mut usize,
) -> usize {
    if messages.is_empty() || *breakpoint_budget == 0 {
        return 0;
    }

    let Some(system_index) = messages
        .iter()
        .position(|message| message.get("role").and_then(Value::as_str) == Some("system"))
    else {
        return 0;
    };
    let Some(system_message) = messages
        .get_mut(system_index)
        .and_then(Value::as_object_mut)
    else {
        return 0;
    };

    let sections = normalize_system_prompt_sections(
        metadata.and_then(|metadata| metadata.get(SYSTEM_PROMPT_SECTIONS_KEY)),
    );
    let mut blocks = if sections.is_empty() {
        ensure_content_blocks(system_message)
    } else {
        sections
            .iter()
            .map(|section| json!({"type": "text", "text": section.text}))
            .collect::<Vec<_>>()
    };
    if blocks.is_empty() {
        return 0;
    }

    let prefix_char_count = blocks.iter().map(estimate_block_chars).sum();
    if estimate_tokens(prefix_char_count) < token_threshold {
        system_message.insert("content".to_string(), Value::Array(blocks));
        return prefix_char_count;
    }

    let stable_indexes = if sections.is_empty() {
        (0..blocks.len()).collect::<Vec<_>>()
    } else {
        sections
            .iter()
            .enumerate()
            .filter_map(|(index, section)| section.stable.then_some(index))
            .collect::<Vec<_>>()
    };
    if let Some(index) = stable_indexes.last().copied() {
        set_cache_control(&mut blocks[index]);
        *breakpoint_budget = breakpoint_budget.saturating_sub(1);
    }
    system_message.insert("content".to_string(), Value::Array(blocks));
    prefix_char_count
}

fn apply_tool_cache_breakpoint(
    tools: &mut [Value],
    prefix_char_count: usize,
    token_threshold: usize,
    breakpoint_budget: &mut usize,
) -> usize {
    if tools.is_empty() || *breakpoint_budget == 0 {
        return 0;
    }

    let tool_char_count = tools.iter().map(estimate_tool_chars).sum::<usize>();
    if estimate_tokens(prefix_char_count + tool_char_count) < token_threshold {
        return tool_char_count;
    }

    if let Some(tool) = tools.last_mut().and_then(Value::as_object_mut) {
        tool.insert("cache_control".to_string(), cache_control_ephemeral());
        *breakpoint_budget = breakpoint_budget.saturating_sub(1);
    }
    tool_char_count
}

fn apply_history_cache_breakpoint(
    messages: &mut [Value],
    prefix_char_count: usize,
    token_threshold: usize,
    breakpoint_budget: usize,
) {
    if breakpoint_budget == 0 {
        return;
    }

    let Some((message_index, block_index)) = find_history_breakpoint(messages) else {
        return;
    };

    let mut history_char_count = prefix_char_count;
    for (index, message) in messages.iter().enumerate() {
        if message.get("role").and_then(Value::as_str) == Some("system") {
            continue;
        }
        let blocks = content_blocks(message);
        if index < message_index {
            history_char_count += blocks.iter().map(estimate_block_chars).sum::<usize>();
            continue;
        }
        history_char_count += blocks
            .iter()
            .take(block_index + 1)
            .map(estimate_block_chars)
            .sum::<usize>();
        break;
    }

    if estimate_tokens(history_char_count) < token_threshold {
        return;
    }

    let Some(target_message) = messages
        .get_mut(message_index)
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let mut target_blocks = ensure_content_blocks(target_message);
    if let Some(block) = target_blocks.get_mut(block_index) {
        set_cache_control(block);
        target_message.insert("content".to_string(), Value::Array(target_blocks));
    }
}

fn find_history_breakpoint(messages: &[Value]) -> Option<(usize, usize)> {
    let mut fallback = None;
    for message_index in (0..messages.len()).rev() {
        let message = &messages[message_index];
        if message.get("role").and_then(Value::as_str) == Some("system") {
            continue;
        }
        let blocks = content_blocks(message);
        let mut best = None::<(usize, u8)>;
        for block_index in (0..blocks.len()).rev() {
            let block = &blocks[block_index];
            let block_type = block_type(block);
            if is_thinking_block_type(block_type.as_str()) {
                continue;
            }
            if block.get("cache_control").is_some() {
                continue;
            }
            if estimate_block_chars(block) == 0 {
                continue;
            }
            if let Some(priority) = history_priority(&block_type) {
                if best
                    .as_ref()
                    .is_none_or(|(_, existing_priority)| priority < *existing_priority)
                {
                    best = Some((block_index, priority));
                }
                break;
            }
            fallback.get_or_insert((message_index, block_index));
        }
        if let Some((block_index, _)) = best {
            return Some((message_index, block_index));
        }
    }
    fallback
}

fn history_priority(block_type: &str) -> Option<u8> {
    match block_type {
        "tool_result" => Some(0),
        "text" => Some(1),
        _ => None,
    }
}
