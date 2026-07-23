use std::collections::BTreeMap;

use serde_json::Value;

use crate::runtime::sub_agents::types::SubRunLifecycle;
use crate::runtime::{CancellationToken, ExecutionContext};

pub(super) fn project_execution_context(
    parent: Option<&ExecutionContext>,
    lifecycle: &SubRunLifecycle,
    cancellation_token: Option<CancellationToken>,
) -> ExecutionContext {
    let mut metadata = BTreeMap::from([
        (
            "_vv_agent_run_id".to_string(),
            Value::String(lifecycle.run_id.clone()),
        ),
        (
            "_vv_agent_trace_id".to_string(),
            Value::String(lifecycle.trace_id.clone()),
        ),
        (
            "trace_id".to_string(),
            Value::String(lifecycle.trace_id.clone()),
        ),
        (
            "_vv_agent_agent_name".to_string(),
            Value::String(lifecycle.agent_name.clone()),
        ),
        (
            "_vv_agent_session_id".to_string(),
            Value::String(lifecycle.session_id.clone()),
        ),
    ]);
    if !lifecycle.parent_run_id.is_empty() {
        metadata.insert(
            "_vv_agent_parent_run_id".to_string(),
            Value::String(lifecycle.parent_run_id.clone()),
        );
    }
    if !lifecycle.parent_tool_call_id.is_empty() {
        metadata.insert(
            "_vv_agent_parent_tool_call_id".to_string(),
            Value::String(lifecycle.parent_tool_call_id.clone()),
        );
    }
    if let Some(parent) = parent {
        for key in ["_vv_agent_trace_context", "trace_context"] {
            if let Some(value) = parent.metadata.get(key) {
                metadata.insert(key.to_string(), value.clone());
            }
        }
    }
    ExecutionContext {
        cancellation_token,
        event_handler: None,
        checkpoint_store: parent.and_then(|context| context.checkpoint_store.clone()),
        approval_provider: parent.and_then(|context| context.approval_provider.clone()),
        approval_broker: parent.and_then(|context| context.approval_broker.clone()),
        approval_timeout: parent.and_then(|context| context.approval_timeout),
        memory_providers: parent
            .map(|context| context.memory_providers.clone())
            .unwrap_or_default(),
        app_state: parent.and_then(|context| context.app_state.clone()),
        metadata,
        ..ExecutionContext::default()
    }
}
