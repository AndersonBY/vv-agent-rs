use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::llm::{LlmRequest, LlmStreamCallback};
use crate::runtime::RunEventHandler;
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

pub(super) fn effective_model_call_target(
    task: &AgentTask,
    controls: &RuntimeRunControls,
    default_backend: Option<&str>,
) -> (String, String) {
    let metadata = controls
        .execution_context
        .as_ref()
        .map(|context| &context.metadata);
    let backend = metadata
        .and_then(|metadata| identity(metadata, &["_vv_agent_resolved_backend"]))
        .or_else(|| default_backend.map(str::to_string))
        .unwrap_or_else(|| "direct".to_string());
    let model = metadata
        .and_then(|metadata| identity(metadata, &["_vv_agent_resolved_model"]))
        .unwrap_or_else(|| task.model.clone());
    (backend, model)
}

pub(super) fn cycle_stream_callback(
    handler: Option<&RunEventHandler>,
    metadata: &BTreeMap<String, Value>,
    cycle_index: u32,
) -> Option<LlmStreamCallback> {
    handler.map(|handler| {
        let handler = handler.clone();
        let run_id = identity(metadata, &["_vv_agent_run_id", "run_id"])
            .unwrap_or_else(|| "runtime".to_string());
        let trace_id = identity(metadata, &["_vv_agent_trace_id", "trace_id"])
            .unwrap_or_else(|| run_id.clone());
        let agent_name = identity(metadata, &["_vv_agent_agent_name", "agent_name"])
            .unwrap_or_else(|| "runtime".to_string());
        let session_id = identity(metadata, &["_vv_agent_session_id", "session_id"]);
        let parent_run_id = identity(metadata, &["_vv_agent_parent_run_id", "parent_run_id"]);
        let input = identity(metadata, &["_vv_agent_input"]).unwrap_or_default();
        let context = crate::runner::RuntimeEventContext::new(
            run_id, trace_id, agent_name, session_id, input,
        );
        Arc::new(move |event: &BTreeMap<String, Value>| {
            let mut event = event.clone();
            event.insert("cycle".to_string(), Value::from(cycle_index));
            if let Some(mut event) = crate::runner::map_stream_event(&event, &context) {
                if let Some(parent_run_id) = parent_run_id.as_ref() {
                    event = event.with_parent_run_id(parent_run_id);
                }
                handler(&event);
            }
        }) as LlmStreamCallback
    })
}

fn identity(metadata: &BTreeMap<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        metadata
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}
