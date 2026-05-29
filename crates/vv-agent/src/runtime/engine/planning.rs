use serde_json::Value;

use crate::llm::LlmClient;
use crate::runtime::hooks::RuntimeHookManager;
use crate::types::AgentTask;

use super::AgentRuntime;

impl<C: LlmClient + Clone + 'static> AgentRuntime<C> {
    pub(super) fn planned_tool_schemas(&self, task: &AgentTask) -> Vec<Value> {
        self.tool_registry.planned_openai_schemas(task)
    }

    pub(super) fn hook_manager(&self) -> RuntimeHookManager {
        RuntimeHookManager::new(self.hooks.clone())
    }
}
