use serde_json::Value;

#[derive(Debug, Default)]
pub(super) struct StreamingToolCallParts {
    pub(super) id: String,
    pub(super) index: Option<usize>,
    pub(super) name: String,
    pub(super) arguments: String,
    pub(super) extra_content: Option<Value>,
}

pub(super) fn resolve_stream_tool_call_key(
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
