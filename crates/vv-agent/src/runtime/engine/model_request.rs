use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::llm::{LlmRequest, LlmStreamCallback};
use crate::types::{AgentTask, Message};

use super::RuntimeRunControls;

pub(super) fn build_model_request(
    task: &AgentTask,
    controls: &RuntimeRunControls,
    messages: &[Message],
    tool_schemas: &[Value],
) -> LlmRequest {
    let mut request = LlmRequest::new(task.model.clone(), messages.to_vec());
    request.tools = tool_schemas.to_vec();
    let mut request_metadata = task.metadata.clone();
    if let Some(execution_context) = controls.execution_context.as_ref() {
        request_metadata.extend(execution_context.metadata.clone());
    }
    request.metadata = Value::Object(request_metadata.into_iter().collect());
    request.model_settings = task.model_settings.clone();
    request
}

pub(super) fn cycle_stream_callback(
    callback: Option<&LlmStreamCallback>,
    cycle_index: u32,
) -> Option<LlmStreamCallback> {
    callback.map(|callback| {
        let callback = callback.clone();
        Arc::new(move |event: &BTreeMap<String, Value>| {
            let mut event = event.clone();
            event.insert("cycle".to_string(), Value::from(cycle_index));
            callback(&event);
        }) as LlmStreamCallback
    })
}
