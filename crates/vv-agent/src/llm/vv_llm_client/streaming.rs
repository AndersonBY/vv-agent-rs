use std::collections::BTreeMap;
use std::sync::Arc;

use futures_util::StreamExt;
use serde_json::Value;

use crate::llm::{LlmError, LlmStreamCallback};
use crate::types::{LLMResponse, Metadata};

use super::response::{
    completion_payload_for_usage, estimate_missing_usage, from_vv_llm_tool_call, from_vv_llm_usage,
    UsageEstimateContext,
};

#[derive(Debug, Default)]
struct StreamingToolCallParts {
    id: String,
    index: Option<usize>,
    name: String,
    arguments: String,
}

pub(super) async fn collect_vv_llm_stream(
    chat_client: Arc<dyn vv_llm::ChatClient>,
    request: vv_llm::ChatRequest,
    stream_callback: Option<LlmStreamCallback>,
    estimate: Option<UsageEstimateContext>,
) -> Result<LLMResponse, LlmError> {
    let model = request.model.clone();
    let mut stream = chat_client
        .create_stream(request)
        .await
        .map_err(|error| LlmError::Request(error.to_string()))?;
    let mut content = String::new();
    let mut reasoning_content = String::new();
    let mut raw_content = Vec::new();
    let mut usage = None;
    let mut tool_order = Vec::<String>::new();
    let mut tool_calls = BTreeMap::<String, StreamingToolCallParts>::new();
    let mut active_tool_call_key = None::<String>;

    while let Some(delta) = stream.next().await {
        let delta = delta.map_err(|error| LlmError::Request(error.to_string()))?;
        if let Some(delta_usage) = delta.usage {
            usage = Some(delta_usage);
        }
        if let Some(raw) = delta.raw_content {
            collect_raw_content(&mut raw_content, raw);
        }
        if !delta.reasoning_content.is_empty() {
            reasoning_content.push_str(&delta.reasoning_content);
            let reasoning_chars = reasoning_content.chars().count();
            emit_stream_event(
                &stream_callback,
                BTreeMap::from([
                    (
                        "event".to_string(),
                        Value::String("reasoning_delta".to_string()),
                    ),
                    (
                        "reasoning_delta".to_string(),
                        Value::String(delta.reasoning_content),
                    ),
                    (
                        "reasoning_chars".to_string(),
                        Value::from(reasoning_chars as u64),
                    ),
                    (
                        "estimated_tokens".to_string(),
                        Value::from(estimate_stream_tokens(reasoning_chars) as u64),
                    ),
                ]),
            );
        }
        if !delta.content.is_empty() {
            content.push_str(&delta.content);
            let content_chars = content.chars().count();
            emit_stream_event(
                &stream_callback,
                BTreeMap::from([
                    (
                        "event".to_string(),
                        Value::String("assistant_delta".to_string()),
                    ),
                    ("content_delta".to_string(), Value::String(delta.content)),
                    (
                        "content_chars".to_string(),
                        Value::from(content_chars as u64),
                    ),
                    (
                        "estimated_tokens".to_string(),
                        Value::from(estimate_stream_tokens(content_chars) as u64),
                    ),
                ]),
            );
        }
        for (tool_call_index, tool_call_delta) in delta.tool_calls.into_iter().enumerate() {
            let key = resolve_stream_tool_call_key(
                &tool_call_delta,
                tool_call_index,
                active_tool_call_key.as_deref(),
            );
            active_tool_call_key = Some(key.clone());
            if !tool_calls.contains_key(&key) {
                tool_order.push(key.clone());
                tool_calls.insert(
                    key.clone(),
                    StreamingToolCallParts {
                        id: if tool_call_delta.id.is_empty() {
                            key.clone()
                        } else {
                            tool_call_delta.id.clone()
                        },
                        index: tool_call_delta.index,
                        ..StreamingToolCallParts::default()
                    },
                );
            }
            let Some(slot) = tool_calls.get_mut(&key) else {
                continue;
            };
            if !tool_call_delta.id.is_empty() {
                slot.id = tool_call_delta.id.clone();
            }
            if tool_call_delta.index.is_some() {
                slot.index = tool_call_delta.index;
            }
            let had_name = !slot.name.is_empty();
            if !tool_call_delta.name.is_empty() {
                slot.name = tool_call_delta.name.clone();
            }
            if !tool_call_delta.arguments.is_empty() {
                slot.arguments.push_str(&tool_call_delta.arguments);
            }
            if !had_name && !slot.name.is_empty() {
                emit_tool_stream_event(
                    &stream_callback,
                    "tool_call_started",
                    tool_call_index,
                    slot,
                );
            }
            if !tool_call_delta.arguments.is_empty() {
                emit_tool_stream_event(
                    &stream_callback,
                    "tool_call_progress",
                    tool_call_index,
                    slot,
                );
            }
        }
        if delta.done {
            break;
        }
    }

    let mut raw = Metadata::new();
    raw.insert("model".to_string(), Value::String(model));
    raw.insert("stream_collected".to_string(), Value::Bool(true));
    if !reasoning_content.is_empty() {
        raw.insert(
            "reasoning_content".to_string(),
            Value::String(reasoning_content),
        );
    }
    if !raw_content.is_empty() {
        raw.insert("raw_content".to_string(), Value::Array(raw_content));
    }
    let completion_payload = completion_payload_for_usage(
        &content,
        &tool_order
            .iter()
            .filter_map(|key| {
                tool_calls.get(key).map(|parts| {
                    vv_llm::ToolCall::function(&parts.id, &parts.name, &parts.arguments)
                })
            })
            .collect::<Vec<_>>(),
    );
    let token_usage = usage
        .map(from_vv_llm_usage)
        .unwrap_or_else(|| estimate_missing_usage(&completion_payload, &[], estimate));
    raw.insert("usage".to_string(), token_usage.raw.clone());
    Ok(LLMResponse {
        content,
        tool_calls: tool_order
            .into_iter()
            .filter_map(|key| tool_calls.remove(&key))
            .filter(|parts| !parts.name.is_empty())
            .enumerate()
            .map(|(index, parts)| {
                from_vv_llm_tool_call(
                    vv_llm::ToolCall::function(parts.id, parts.name, parts.arguments),
                    index,
                )
            })
            .collect(),
        raw,
        token_usage,
    })
}

fn resolve_stream_tool_call_key(
    tool_call: &vv_llm::ToolCall,
    index: usize,
    active_tool_call_key: Option<&str>,
) -> String {
    if !tool_call.id.is_empty() {
        return tool_call.id.clone();
    }
    if let Some(active) = active_tool_call_key {
        return active.to_string();
    }
    format!("call_stream_{index}")
}

fn collect_raw_content(blocks: &mut Vec<Value>, chunk: Value) {
    match chunk {
        Value::Array(items) => {
            for item in items {
                collect_raw_content(blocks, item);
            }
        }
        Value::Object(object) => collect_raw_content_object(blocks, object),
        _ => {}
    }
}

fn collect_raw_content_object(blocks: &mut Vec<Value>, object: serde_json::Map<String, Value>) {
    let chunk_type = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match chunk_type {
        "thinking_delta" => {
            let index = find_or_create_raw_block(
                blocks,
                "thinking",
                &[("thinking", ""), ("signature", "")],
            );
            append_raw_block_string(blocks, index, "thinking", object.get("thinking"));
        }
        "signature_delta" => {
            let index = find_or_create_raw_block(
                blocks,
                "thinking",
                &[("thinking", ""), ("signature", "")],
            );
            append_raw_block_string(blocks, index, "signature", object.get("signature"));
        }
        "text_delta" => {
            let index = find_or_create_raw_block(blocks, "text", &[("text", "")]);
            append_raw_block_string(blocks, index, "text", object.get("text"));
        }
        "input_json_delta" => {}
        "thinking" | "text" | "tool_use" => {
            if !raw_block_exists(blocks, &object) {
                blocks.push(Value::Object(object));
            }
        }
        _ => blocks.push(Value::Object(object)),
    }
}

fn find_or_create_raw_block(
    blocks: &mut Vec<Value>,
    block_type: &str,
    defaults: &[(&str, &str)],
) -> usize {
    if let Some(index) = blocks.iter().position(|block| {
        block
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|candidate| candidate == block_type)
    }) {
        return index;
    }

    let mut block = serde_json::Map::new();
    block.insert("type".to_string(), Value::String(block_type.to_string()));
    for (key, value) in defaults {
        block.insert((*key).to_string(), Value::String((*value).to_string()));
    }
    blocks.push(Value::Object(block));
    blocks.len() - 1
}

fn append_raw_block_string(
    blocks: &mut [Value],
    index: usize,
    key: &str,
    addition: Option<&Value>,
) {
    let addition = addition.and_then(Value::as_str).unwrap_or_default();
    let Some(block) = blocks.get_mut(index).and_then(Value::as_object_mut) else {
        return;
    };
    let mut value = block
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    value.push_str(addition);
    block.insert(key.to_string(), Value::String(value));
}

fn raw_block_exists(blocks: &[Value], candidate: &serde_json::Map<String, Value>) -> bool {
    let candidate_type = candidate.get("type");
    let candidate_id = candidate.get("id");
    blocks.iter().any(|block| {
        let Some(block) = block.as_object() else {
            return false;
        };
        if block.get("type") != candidate_type {
            return false;
        }
        if let Some(candidate_id) = candidate_id {
            return block.get("id") == Some(candidate_id);
        }
        block == candidate
    })
}

fn emit_stream_event(stream_callback: &Option<LlmStreamCallback>, event: BTreeMap<String, Value>) {
    if let Some(callback) = stream_callback {
        callback(&event);
    }
}

fn emit_tool_stream_event(
    stream_callback: &Option<LlmStreamCallback>,
    event_name: &str,
    default_tool_call_index: usize,
    tool_call: &StreamingToolCallParts,
) {
    let tool_call_index = tool_call.index.unwrap_or(default_tool_call_index);
    emit_stream_event(
        stream_callback,
        BTreeMap::from([
            ("event".to_string(), Value::String(event_name.to_string())),
            (
                "tool_call_id".to_string(),
                Value::String(tool_call.id.clone()),
            ),
            (
                "tool_call_index".to_string(),
                Value::from(tool_call_index as u64),
            ),
            (
                "function_name".to_string(),
                Value::String(tool_call.name.clone()),
            ),
            (
                "arguments_chars".to_string(),
                Value::from(tool_call.arguments.chars().count() as u64),
            ),
            (
                "estimated_tokens".to_string(),
                Value::from(estimate_stream_tokens(tool_call.arguments.chars().count()) as u64),
            ),
        ]),
    );
}

fn estimate_stream_tokens(char_count: usize) -> usize {
    if char_count == 0 {
        0
    } else {
        char_count.div_ceil(4)
    }
}
