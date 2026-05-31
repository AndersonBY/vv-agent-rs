use std::sync::Arc;

use crate::runtime::{ExecutionContext, RuntimeRunControls};
use crate::sdk::session::AgentSessionRunRequest;
use crate::workspace::{LocalWorkspaceBackend, WorkspaceBackend};

fn execution_context_from_request(request: &AgentSessionRunRequest) -> Option<ExecutionContext> {
    request
        .stream_callback
        .clone()
        .map(|callback| ExecutionContext::default().with_stream_callback(callback))
}

pub(super) fn run_controls_from_request(request: &AgentSessionRunRequest) -> RuntimeRunControls {
    RuntimeRunControls {
        log_handler: request.runtime_event_handler.clone(),
        before_cycle_messages: request.before_cycle_messages.clone(),
        interruption_messages: request.interruption_messages.clone(),
        steering_queue: request.steering_queue.clone(),
        cancellation_token: request.cancellation_token.clone(),
        execution_context: execution_context_from_request(request),
        workspace: request.workspace.clone(),
        workspace_backend: request.workspace.as_ref().map(|workspace| {
            Arc::new(LocalWorkspaceBackend::new(workspace.clone())) as Arc<dyn WorkspaceBackend>
        }),
        model_provider: None,
        sub_task_manager: request.sub_task_manager.clone(),
    }
}
