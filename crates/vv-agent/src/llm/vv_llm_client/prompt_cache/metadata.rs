use serde_json::Value;

use crate::llm::LlmRequest;
use crate::types::MessageRole;

pub(in crate::llm::vv_llm_client) fn request_metadata_for_prompt_cache(
    request: &LlmRequest,
) -> Value {
    let mut metadata = serde_json::Map::new();
    if let Some(system_metadata) = request
        .messages
        .iter()
        .find(|message| message.role == MessageRole::System)
        .map(|message| &message.metadata)
        .filter(|metadata| !metadata.is_empty())
    {
        metadata.extend(system_metadata.clone());
    }
    if let Some(request_metadata) = request.metadata.as_object() {
        metadata.extend(request_metadata.clone());
    }
    Value::Object(metadata)
}
