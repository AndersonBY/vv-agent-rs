use serde_json::Value;

pub(super) fn cache_content_to_vv_llm_content(content: &Value) -> Vec<vv_llm::MessageContent> {
    match content {
        Value::Array(items) => items
            .iter()
            .filter_map(cache_block_to_vv_llm_content)
            .collect(),
        Value::String(text) => vec![vv_llm::MessageContent::text(text.clone())],
        _ => Vec::new(),
    }
}

fn cache_block_to_vv_llm_content(block: &Value) -> Option<vv_llm::MessageContent> {
    let object = block.as_object()?;
    let block_type = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("text")
        .to_ascii_lowercase();
    match block_type.as_str() {
        "text" => Some(vv_llm_text_content_from_cache_block(object)),
        "image_url" => object
            .get("image_url")
            .and_then(|image_url| image_url.get("url"))
            .and_then(Value::as_str)
            .map(|url| vv_llm::MessageContent::ImageUrl {
                url: url.to_string(),
            }),
        "tool_result" => {
            let text = cache_block_content_text(object.get("content"));
            Some(vv_llm_text_content_with_optional_cache_control(
                text,
                object.get("cache_control").cloned(),
            ))
        }
        _ => None,
    }
}

fn vv_llm_text_content_from_cache_block(
    object: &serde_json::Map<String, Value>,
) -> vv_llm::MessageContent {
    let text = object
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    vv_llm_text_content_with_optional_cache_control(text, object.get("cache_control").cloned())
}

fn vv_llm_text_content_with_optional_cache_control(
    text: String,
    cache_control: Option<Value>,
) -> vv_llm::MessageContent {
    match cache_control {
        Some(cache_control) => vv_llm::MessageContent::text_with_cache_control(text, cache_control),
        None => vv_llm::MessageContent::text(text),
    }
}

fn cache_block_content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(_)) | Some(Value::Object(_)) => {
            serde_json::to_string(content.unwrap()).unwrap_or_default()
        }
        Some(Value::Bool(value)) => value.to_string(),
        Some(Value::Number(value)) => value.to_string(),
        Some(Value::Null) | None => String::new(),
    }
}
