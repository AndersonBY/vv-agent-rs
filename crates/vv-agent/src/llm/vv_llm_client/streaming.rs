use std::collections::BTreeMap;
use std::sync::Arc;

use futures_util::StreamExt;
use serde_json::Value;

use crate::llm::LlmStreamCallback;
use crate::types::{LLMResponse, Metadata};

use super::response::{
    completion_payload_for_usage, estimate_missing_usage, from_vv_llm_tool_call, from_vv_llm_usage,
    merge_tool_call_extra_content, UsageEstimateContext,
};
use super::EndpointAttemptError;

mod events;
mod raw_content;
mod tool_calls;

use events::{emit_assistant_delta_event, emit_reasoning_delta_event, emit_tool_stream_event};
use raw_content::collect_raw_content;
use tool_calls::{resolve_stream_tool_call_key, StreamingToolCallParts};

pub(super) async fn collect_vv_llm_stream(
    chat_client: Arc<dyn vv_llm::ChatClient>,
    request: vv_llm::ChatRequest,
    stream_callback: Option<LlmStreamCallback>,
    estimate: Option<UsageEstimateContext>,
) -> Result<LLMResponse, EndpointAttemptError> {
    let model = request.model.clone();
    let mut stream = chat_client
        .create_stream(request)
        .await
        .map_err(EndpointAttemptError::from_provider)?;
    let mut content = String::new();
    let mut reasoning_content = String::new();
    let mut raw_content = Vec::new();
    let mut usage = None;
    let mut tool_order = Vec::<String>::new();
    let mut tool_calls = BTreeMap::<String, StreamingToolCallParts>::new();
    let mut active_tool_call_key = None::<String>;

    while let Some(delta) = stream.next().await {
        let delta = delta.map_err(EndpointAttemptError::from_provider)?;
        if let Some(delta_usage) = delta.usage {
            usage = Some(delta_usage);
        }
        if let Some(raw) = delta.raw_content {
            collect_raw_content(&mut raw_content, raw);
        }
        if !delta.reasoning_content.is_empty() {
            reasoning_content.push_str(&delta.reasoning_content);
            let reasoning_chars = reasoning_content.chars().count();
            emit_reasoning_delta_event(&stream_callback, delta.reasoning_content, reasoning_chars);
        }
        if !delta.content.is_empty() {
            content.push_str(&delta.content);
            let content_chars = content.chars().count();
            emit_assistant_delta_event(&stream_callback, delta.content, content_chars);
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
            if let Some(extra_content) = tool_call_delta.extra_content {
                slot.extra_content = Some(match slot.extra_content.take() {
                    Some(existing) => merge_tool_call_extra_content(existing, extra_content),
                    None => extra_content,
                });
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
    let collected_tool_calls = tool_order
        .iter()
        .filter_map(|key| tool_calls.get(key))
        .filter(|parts| !parts.name.is_empty())
        .map(stream_parts_to_tool_call)
        .collect::<Vec<_>>();
    let completion_payload = completion_payload_for_usage(&content, &collected_tool_calls);
    let token_usage = usage
        .map(from_vv_llm_usage)
        .unwrap_or_else(|| estimate_missing_usage(&completion_payload, &[], estimate));
    raw.insert("usage".to_string(), token_usage.raw.clone());
    Ok(LLMResponse {
        content,
        tool_calls: collected_tool_calls
            .into_iter()
            .enumerate()
            .map(|(index, tool_call)| from_vv_llm_tool_call(tool_call, index))
            .collect(),
        raw,
        token_usage,
    })
}

fn stream_parts_to_tool_call(parts: &StreamingToolCallParts) -> vv_llm::ToolCall {
    vv_llm::ToolCall {
        id: parts.id.clone(),
        name: parts.name.clone(),
        arguments: parts.arguments.clone(),
        index: parts.index,
        extra_content: parts.extra_content.clone(),
    }
}
