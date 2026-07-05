use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use crate::types::{Message, MessageRole};

pub const CLEARED_MARKER: &str = "[Old tool result content cleared by microcompact]";

pub const COMPACTABLE_TOOLS: &[&str] = &[
    "read_file",
    "write_file",
    "edit_file",
    "list_files",
    "workspace_grep",
    "bash",
    "file_info",
];

#[derive(Debug, Clone, PartialEq)]
pub struct MicrocompactConfig {
    pub trigger_ratio: f64,
    pub keep_recent_cycles: usize,
    pub min_result_length: usize,
    pub compactable_tools: Option<BTreeSet<String>>,
}

impl Default for MicrocompactConfig {
    fn default() -> Self {
        Self {
            trigger_ratio: 0.75,
            keep_recent_cycles: 3,
            min_result_length: 500,
            compactable_tools: None,
        }
    }
}

pub fn microcompact(
    messages: &[Message],
    current_cycle: u32,
    config: &MicrocompactConfig,
) -> (Vec<Message>, usize) {
    if messages.is_empty() {
        return (Vec::new(), 0);
    }

    let tool_call_names = build_tool_call_name_map(messages);
    let inferred_cycles = infer_message_cycles(messages);
    let max_inferred_cycle = inferred_cycles.last().copied().unwrap_or_default();
    let effective_current_cycle = current_cycle.min(max_inferred_cycle.saturating_add(1));
    let protected_cycle = effective_current_cycle.saturating_sub(config.keep_recent_cycles as u32);
    let compactable_tools = config
        .compactable_tools
        .clone()
        .unwrap_or_else(default_compactable_tools);

    let mut cleared = 0;
    let mut updated = Vec::with_capacity(messages.len());
    for (message, inferred_cycle) in messages.iter().zip(inferred_cycles) {
        if should_clear_message(
            message,
            inferred_cycle,
            protected_cycle,
            config.min_result_length,
            &compactable_tools,
            &tool_call_names,
        ) {
            updated.push(replace_content(message));
            cleared += 1;
        } else {
            updated.push(message.clone());
        }
    }
    (updated, cleared)
}

pub fn is_microcompacted_tool_content(content: &str) -> bool {
    content.starts_with(CLEARED_MARKER)
}

fn default_compactable_tools() -> BTreeSet<String> {
    COMPACTABLE_TOOLS
        .iter()
        .map(|tool| (*tool).to_string())
        .collect()
}

fn build_tool_call_name_map(messages: &[Message]) -> BTreeMap<String, String> {
    let mut tool_call_names = BTreeMap::new();
    for message in messages {
        if message.role != MessageRole::Assistant {
            continue;
        }
        for tool_call in &message.tool_calls {
            tool_call_names.insert(tool_call.id.clone(), tool_call.name.clone());
        }
    }
    tool_call_names
}

fn infer_message_cycles(messages: &[Message]) -> Vec<u32> {
    let mut current_cycle = 0;
    let mut inferred = Vec::with_capacity(messages.len());
    for message in messages {
        if message.role == MessageRole::Assistant {
            current_cycle += 1;
        }
        inferred.push(current_cycle);
    }
    inferred
}

fn should_clear_message(
    message: &Message,
    inferred_cycle: u32,
    protected_cycle: u32,
    min_result_length: usize,
    compactable_tools: &BTreeSet<String>,
    tool_call_names: &BTreeMap<String, String>,
) -> bool {
    if message.role != MessageRole::Tool {
        return false;
    }
    if inferred_cycle >= protected_cycle {
        return false;
    }
    if message.content.len() <= min_result_length.max(1) {
        return false;
    }
    if is_microcompacted_tool_content(&message.content) {
        return false;
    }
    let Some(tool_call_id) = message.tool_call_id.as_deref() else {
        return false;
    };
    let Some(tool_name) = tool_call_names.get(tool_call_id) else {
        return false;
    };
    compactable_tools.contains(tool_name)
}

fn replace_content(message: &Message) -> Message {
    let mut updated = message.clone();
    updated
        .metadata
        .insert("microcompacted".to_string(), json!(true));
    updated.metadata.insert(
        "microcompact_original_chars".to_string(),
        json!(message.content.len()),
    );
    updated.content = CLEARED_MARKER.to_string();
    updated
}
