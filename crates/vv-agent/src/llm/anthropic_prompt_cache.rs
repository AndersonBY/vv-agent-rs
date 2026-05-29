use serde_json::{json, Value};

mod blocks;
mod breakpoints;
mod estimate;
mod model;
mod sections;

pub const SYSTEM_PROMPT_SECTIONS_KEY: &str = "system_prompt_sections";
pub const PROMPT_CACHE_ENABLED_KEY: &str = "anthropic_prompt_cache_enabled";

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

    breakpoints::apply_cache_breakpoints(
        &mut planned_messages,
        &mut planned_tools,
        request_metadata,
        model::minimum_cacheable_tokens(&normalized_model),
    );

    (planned_messages, planned_tools, planned_extra_body)
}
