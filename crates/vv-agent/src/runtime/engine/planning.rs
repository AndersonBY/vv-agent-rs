use futures_util::FutureExt;
use serde_json::Value;

use crate::llm::LlmClient;
use crate::runtime::hooks::RuntimeHookManager;
use crate::tools::ToolError;
use crate::types::AgentTask;

use super::AgentRuntime;

impl<C: LlmClient + Clone + 'static> AgentRuntime<C> {
    pub(super) fn planned_tool_schemas(&self, task: &AgentTask) -> Vec<Value> {
        self.tool_registry
            .planned_openai_schemas_with_policy(task, self.tool_policy.as_ref())
    }

    pub(super) fn hook_manager(&self) -> RuntimeHookManager {
        RuntimeHookManager::new(self.hooks.clone())
    }
}

pub(super) fn block_on_engine_tool_run<'a, T>(
    future: impl std::future::Future<Output = Result<T, ToolError>> + 'a,
) -> Result<T, ToolError> {
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
