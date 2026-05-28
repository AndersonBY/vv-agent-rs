use serde_json::{json, Map, Value};

pub const SYSTEM_PROMPT_SECTIONS_KEY: &str = "system_prompt_sections";
pub const PROMPT_CACHE_ENABLED_KEY: &str = "anthropic_prompt_cache_enabled";

const MAX_BREAKPOINTS: usize = 4;
const THINKING_BLOCK_TYPES: &[&str] = &["thinking", "redacted_thinking"];

pub fn cache_control_ephemeral() -> Value {
    json!({"type": "ephemeral"})
}

#[allow(non_snake_case)]
pub fn CACHE_CONTROL_EPHEMERAL() -> Value {
    cache_control_ephemeral()
}

pub fn apply_claude_prompt_cache(
    endpoint_type: &str,
    model: &str,
    messages: &[Value],
    tools: &[Value],
    extra_body: Option<&Value>,
    metadata: Option<&Value>,
) -> (Vec<Value>, Vec<Value>, Option<Value>) {
    let normalized_endpoint = endpoint_type.trim().to_ascii_lowercase();
    let normalized_model = model.trim().to_ascii_lowercase();
    let request_metadata = metadata.and_then(Value::as_object);

    if !matches!(
        normalized_endpoint.as_str(),
        "anthropic" | "anthropic_vertex"
    ) {
        return (messages.to_vec(), tools.to_vec(), extra_body.cloned());
    }
    if !normalized_model.starts_with("claude") {
        return (messages.to_vec(), tools.to_vec(), extra_body.cloned());
    }
    if request_metadata
        .and_then(|metadata| metadata.get(PROMPT_CACHE_ENABLED_KEY))
        .and_then(Value::as_bool)
        == Some(false)
    {
        return (messages.to_vec(), tools.to_vec(), extra_body.cloned());
    }

    let mut planned_messages = messages.to_vec();
    let mut planned_tools = tools.to_vec();
    let planned_extra_body = extra_body.filter(|value| value.is_object()).cloned();

    let token_threshold = minimum_cacheable_tokens(&normalized_model);
    let mut breakpoint_budget = MAX_BREAKPOINTS;

    let system_char_count = apply_system_cache_breakpoint(
        &mut planned_messages,
        request_metadata,
        token_threshold,
        &mut breakpoint_budget,
    );
    let tool_char_count = apply_tool_cache_breakpoint(
        &mut planned_tools,
        system_char_count,
        token_threshold,
        &mut breakpoint_budget,
    );
    apply_history_cache_breakpoint(
        &mut planned_messages,
        system_char_count + tool_char_count,
        token_threshold,
        breakpoint_budget,
    );

    (planned_messages, planned_tools, planned_extra_body)
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
            if THINKING_BLOCK_TYPES.contains(&block_type.as_str()) {
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

fn ensure_content_blocks(message: &mut Map<String, Value>) -> Vec<Value> {
    match message.get("content") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::Object(_) => Some(item.clone()),
                Value::String(text) => Some(json!({"type": "text", "text": text})),
                _ => None,
            })
            .collect(),
        Some(Value::String(text)) => vec![json!({"type": "text", "text": text})],
        _ => Vec::new(),
    }
}

fn content_blocks(message: &Value) -> Vec<Value> {
    match message.get("content") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::Object(_) => Some(item.clone()),
                Value::String(text) => Some(json!({"type": "text", "text": text})),
                _ => None,
            })
            .collect(),
        Some(Value::String(text)) => vec![json!({"type": "text", "text": text})],
        _ => Vec::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SystemPromptSection {
    text: String,
    stable: bool,
}

fn normalize_system_prompt_sections(raw: Option<&Value>) -> Vec<SystemPromptSection> {
    let Some(items) = raw.and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let object = item.as_object()?;
            let text = value_to_string(object.get("text")).trim().to_string();
            if text.is_empty() {
                return None;
            }
            Some(SystemPromptSection {
                text,
                stable: object.get("stable").is_none_or(value_truthy),
            })
        })
        .collect()
}

fn set_cache_control(block: &mut Value) {
    if let Some(object) = block.as_object_mut() {
        object.insert("cache_control".to_string(), cache_control_ephemeral());
    }
}

fn estimate_tokens(char_count: usize) -> usize {
    if char_count == 0 {
        0
    } else {
        char_count.div_ceil(4)
    }
}

fn estimate_tool_chars(tool: &Value) -> usize {
    sorted_json(tool).len()
}

fn estimate_block_chars(block: &Value) -> usize {
    let block_type = block_type(block);
    match block_type.as_str() {
        "text" => value_to_string(block.get("text")).chars().count(),
        "tool_result" => json_string(block.get("content").unwrap_or(&Value::Null)).len(),
        "tool_use" => {
            value_to_string(block.get("name")).chars().count()
                + json_string(block.get("input").unwrap_or(&Value::Null)).len()
        }
        candidate if THINKING_BLOCK_TYPES.contains(&candidate) => 0,
        _ => json_string(block).len(),
    }
}

fn block_type(block: &Value) -> String {
    value_to_string(block.get("type"))
        .trim()
        .to_ascii_lowercase()
        .if_empty("text")
}

fn history_priority(block_type: &str) -> Option<u8> {
    match block_type {
        "tool_result" => Some(0),
        "text" => Some(1),
        _ => None,
    }
}

fn value_to_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Bool(value)) => value.to_string(),
        Some(Value::Number(value)) => value.to_string(),
        Some(Value::Array(_)) | Some(Value::Object(_)) => {
            json_string(value.unwrap_or(&Value::Null))
        }
        Some(Value::Null) | None => String::new(),
    }
}

fn value_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().is_some_and(|value| value != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(object) => !object.is_empty(),
    }
}

fn json_string(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn sorted_json(value: &Value) -> String {
    serde_json::to_string(&sort_value(value)).unwrap_or_default()
}

fn sort_value(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(sort_value).collect()),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| (key.clone(), sort_value(value)))
                .collect(),
        ),
        value => value.clone(),
    }
}

fn minimum_cacheable_tokens(model: &str) -> usize {
    if model.contains("opus-4-6") || model.contains("opus-4-5") {
        return 4096;
    }
    if model.contains("haiku-4-5") {
        return 4096;
    }
    if model.contains("haiku") {
        return 2048;
    }
    1024
}

trait EmptyStringDefault {
    fn if_empty(self, default: &str) -> String;
}

impl EmptyStringDefault for String {
    fn if_empty(self, default: &str) -> String {
        if self.is_empty() {
            default.to_string()
        } else {
            self
        }
    }
}
