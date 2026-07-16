use futures_util::FutureExt;
use serde_json::Value;

use crate::llm::LlmClient;
use crate::runtime::hooks::RuntimeHookManager;
use crate::tools::ToolError;
use crate::types::AgentTask;
use crate::types::ToolExecutionResult;

use super::AgentRuntime;

impl<C: LlmClient + Clone + 'static> AgentRuntime<C> {
    pub(super) fn planned_tool_schemas(&self, task: &AgentTask) -> Vec<Value> {
        self.tool_registry.planned_openai_schemas(task)
    }

    pub(super) fn hook_manager(&self) -> RuntimeHookManager {
        RuntimeHookManager::new(self.hooks.clone())
    }
}

pub(super) fn block_on_engine_tool_run<'a>(
    future: impl std::future::Future<Output = Result<ToolExecutionResult, ToolError>> + 'a,
) -> Result<ToolExecutionResult, ToolError> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
            tokio::task::block_in_place(|| handle.block_on(future))
        } else {
            future.now_or_never().unwrap_or_else(|| {
                Err(ToolError::new(
                    "tool future cannot be driven from a current-thread runtime",
                ))
            })
        }
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| ToolError::new(error.to_string()))?
            .block_on(future)
    }
}
