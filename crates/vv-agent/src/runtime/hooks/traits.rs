use crate::types::{LLMResponse, Message, ToolExecutionResult};

use super::events::{
    AfterLlmEvent, AfterToolCallEvent, BeforeLlmEvent, BeforeMemoryCompactEvent,
    BeforeToolCallEvent,
};
use super::patches::{BeforeLlmPatch, BeforeToolCallPatch};

pub trait RuntimeHook: Send + Sync {
    fn before_memory_compact(&self, _event: BeforeMemoryCompactEvent<'_>) -> Option<Vec<Message>> {
        None
    }

    fn before_llm(&self, _event: BeforeLlmEvent<'_>) -> Option<BeforeLlmPatch> {
        None
    }

    fn after_llm(&self, _event: AfterLlmEvent<'_>) -> Option<LLMResponse> {
        None
    }

    fn before_tool_call(&self, _event: BeforeToolCallEvent<'_>) -> Option<BeforeToolCallPatch> {
        None
    }

    fn after_tool_call(&self, _event: AfterToolCallEvent<'_>) -> Option<ToolExecutionResult> {
        None
    }
}
