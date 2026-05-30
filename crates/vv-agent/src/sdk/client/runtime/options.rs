use std::sync::{Arc, Mutex};

use crate::llm::LlmClient;
use crate::runtime::AgentRuntime;
use crate::sdk::types::AgentSDKOptions;
use crate::workspace::LocalWorkspaceBackend;

pub(in crate::sdk::client) fn configure_runtime_from_options<C: LlmClient + Clone + 'static>(
    runtime: &mut AgentRuntime<C>,
    options: &AgentSDKOptions,
) {
    if let Some(factory) = &options.tool_registry_factory {
        runtime.tool_registry = factory();
    }
    if let Some(execution_backend) = &options.execution_backend {
        runtime.execution_backend = execution_backend.clone();
    }
    if let Some(log_handler) = &options.log_handler {
        let option_handler = log_handler.clone();
        let previous_handler = runtime.log_handler.take();
        runtime.log_handler = Some(Arc::new(Mutex::new(Box::new(move |event, payload| {
            if let Some(previous_handler) = &previous_handler {
                if let Ok(mut previous_handler) = previous_handler.lock() {
                    previous_handler(event, payload);
                }
            }
            option_handler(event, payload);
        }))));
    }
    if runtime.log_preview_chars.is_none() {
        runtime.log_preview_chars = options.log_preview_chars;
    }
    if runtime.default_workspace.is_none() {
        let workspace = options.workspace.clone();
        runtime.default_workspace = Some(workspace.clone());
        runtime.workspace_backend = Arc::new(LocalWorkspaceBackend::new(workspace));
    }
    runtime.hooks.extend(options.runtime_hooks.clone());
}
