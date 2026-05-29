use crate::memory::message_sanitizer::filter_empty_assistant_messages;
use crate::types::{Message, MessageRole};

pub(super) fn normalize_compaction_messages(
    messages: &[Message],
    tool_calls_keep_last: usize,
    assistant_no_tool_keep_last: usize,
) -> (Vec<Message>, bool) {
    let (messages, stripped) = strip_stale_tool_calls(messages, tool_calls_keep_last);
    let (messages, normalized) = normalize_orphan_tool_messages(&messages);
    let (messages, collapsed) =
        collapse_assistant_no_tool_messages(&messages, assistant_no_tool_keep_last);
    let sanitized = filter_empty_assistant_messages(&messages);
    let sanitized_changed = sanitized.len() != messages.len()
        || sanitized
            .iter()
            .zip(messages.iter())
            .any(|(left, right)| left != right);
    (
        sanitized,
        stripped || normalized || collapsed || sanitized_changed,
    )
}

fn strip_stale_tool_calls(messages: &[Message], keep_count: usize) -> (Vec<Message>, bool) {
    let tool_call_indices = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            (message.role == MessageRole::Assistant && !message.tool_calls.is_empty())
                .then_some(index)
        })
        .collect::<Vec<_>>();
    let keep_indices = if keep_count == 0 {
        Vec::new()
    } else {
        tool_call_indices
            .iter()
            .rev()
            .take(keep_count)
            .copied()
            .collect::<Vec<_>>()
    };

    let mut changed = false;
    let mut stripped = Vec::with_capacity(messages.len());
    for (index, message) in messages.iter().enumerate() {
        if message.role == MessageRole::Assistant
            && !message.tool_calls.is_empty()
            && !keep_indices.contains(&index)
        {
            changed = true;
            let mut updated = message.clone();
            updated.tool_calls.clear();
            if updated.content.trim().is_empty() {
                continue;
            }
            stripped.push(updated);
        } else {
            stripped.push(message.clone());
        }
    }
    (stripped, changed)
}

fn normalize_orphan_tool_messages(messages: &[Message]) -> (Vec<Message>, bool) {
    let mut changed = false;
    let mut pending_tool_calls = std::collections::BTreeMap::<String, usize>::new();
    let mut normalized = Vec::with_capacity(messages.len());
    for message in messages {
        if message.role == MessageRole::Assistant && !message.tool_calls.is_empty() {
            for tool_call in &message.tool_calls {
                let tool_call_id = tool_call.id.trim();
                if tool_call_id.is_empty() {
                    continue;
                }
                *pending_tool_calls
                    .entry(tool_call_id.to_string())
                    .or_default() += 1;
            }
            normalized.push(message.clone());
            continue;
        }

        if message.role == MessageRole::Tool {
            let tool_call_id = message.tool_call_id.as_deref().unwrap_or_default().trim();
            if tool_call_id.is_empty() {
                changed = true;
                continue;
            }
            let remaining = pending_tool_calls.get(tool_call_id).copied().unwrap_or(0);
            if remaining == 0 {
                changed = true;
                continue;
            }
            pending_tool_calls.insert(tool_call_id.to_string(), remaining - 1);
        }
        normalized.push(message.clone());
    }
    (normalized, changed)
}

fn collapse_assistant_no_tool_messages(
    messages: &[Message],
    keep_last: usize,
) -> (Vec<Message>, bool) {
    if keep_last == 0 {
        return (messages.to_vec(), false);
    }
    let mut changed = false;
    let mut collapsed = Vec::with_capacity(messages.len());
    let mut run_buffer = Vec::<Message>::new();
    for message in messages {
        if message.role == MessageRole::Assistant && message.tool_calls.is_empty() {
            run_buffer.push(message.clone());
            continue;
        }
        flush_assistant_run(&mut collapsed, &mut run_buffer, keep_last, &mut changed);
        collapsed.push(message.clone());
    }
    flush_assistant_run(&mut collapsed, &mut run_buffer, keep_last, &mut changed);
    (collapsed, changed)
}

fn flush_assistant_run(
    collapsed: &mut Vec<Message>,
    run_buffer: &mut Vec<Message>,
    keep_last: usize,
    changed: &mut bool,
) {
    if run_buffer.is_empty() {
        return;
    }
    if run_buffer.len() > keep_last {
        *changed = true;
        let start = run_buffer.len() - keep_last;
        collapsed.extend(run_buffer[start..].iter().cloned());
    } else {
        collapsed.append(run_buffer);
        return;
    }
    run_buffer.clear();
}
