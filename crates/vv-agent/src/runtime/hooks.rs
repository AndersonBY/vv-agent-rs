use std::collections::BTreeMap;

use serde_json::Value;

use crate::tools::ToolContext;
use crate::types::{AgentTask, LLMResponse, Message, ToolCall, ToolExecutionResult};

pub struct BeforeLlmEvent<'a> {
    pub task: &'a AgentTask,
    pub cycle_index: u32,
    pub messages: &'a [Message],
    pub tool_schemas: &'a [Value],
    pub shared_state: &'a BTreeMap<String, Value>,
}

pub struct AfterLlmEvent<'a> {
    pub task: &'a AgentTask,
    pub cycle_index: u32,
    pub messages: &'a [Message],
    pub tool_schemas: &'a [Value],
    pub response: &'a LLMResponse,
    pub shared_state: &'a BTreeMap<String, Value>,
}

pub struct BeforeToolCallEvent<'a> {
    pub task: &'a AgentTask,
    pub cycle_index: u32,
    pub call: &'a ToolCall,
    pub context: &'a ToolContext,
}

pub struct AfterToolCallEvent<'a> {
    pub task: &'a AgentTask,
    pub cycle_index: u32,
    pub call: &'a ToolCall,
    pub context: &'a ToolContext,
    pub result: &'a ToolExecutionResult,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BeforeLlmPatch {
    pub messages: Option<Vec<Message>>,
    pub tool_schemas: Option<Vec<Value>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BeforeToolCallPatch {
    pub call: Option<ToolCall>,
    pub result: Option<ToolExecutionResult>,
}

pub trait RuntimeHook: Send + Sync {
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

#[derive(Default)]
pub struct RuntimeHookManager {
    hooks: Vec<std::sync::Arc<dyn RuntimeHook>>,
}

impl RuntimeHookManager {
    pub fn new(hooks: Vec<std::sync::Arc<dyn RuntimeHook>>) -> Self {
        Self { hooks }
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    pub fn apply_before_llm(
        &self,
        task: &AgentTask,
        cycle_index: u32,
        messages: Vec<Message>,
        tool_schemas: Vec<Value>,
        shared_state: &BTreeMap<String, Value>,
    ) -> (Vec<Message>, Vec<Value>) {
        let mut current_messages = messages;
        let mut current_tool_schemas = tool_schemas;
        for hook in &self.hooks {
            let patch = hook.before_llm(BeforeLlmEvent {
                task,
                cycle_index,
                messages: &current_messages,
                tool_schemas: &current_tool_schemas,
                shared_state,
            });
            let Some(patch) = patch else {
                continue;
            };
            if let Some(messages) = patch.messages {
                current_messages = messages;
            }
            if let Some(tool_schemas) = patch.tool_schemas {
                current_tool_schemas = tool_schemas;
            }
        }
        (current_messages, current_tool_schemas)
    }

    pub fn apply_after_llm(
        &self,
        task: &AgentTask,
        cycle_index: u32,
        messages: &[Message],
        tool_schemas: &[Value],
        response: LLMResponse,
        shared_state: &BTreeMap<String, Value>,
    ) -> LLMResponse {
        let mut current = response;
        for hook in &self.hooks {
            if let Some(patched) = hook.after_llm(AfterLlmEvent {
                task,
                cycle_index,
                messages,
                tool_schemas,
                response: &current,
                shared_state,
            }) {
                current = patched;
            }
        }
        current
    }

    pub fn apply_before_tool_call(
        &self,
        task: &AgentTask,
        cycle_index: u32,
        call: ToolCall,
        context: &ToolContext,
    ) -> (ToolCall, Option<ToolExecutionResult>) {
        let mut current_call = call;
        let mut short_circuit = None;
        for hook in &self.hooks {
            let patch = hook.before_tool_call(BeforeToolCallEvent {
                task,
                cycle_index,
                call: &current_call,
                context,
            });
            let Some(patch) = patch else {
                continue;
            };
            if let Some(call) = patch.call {
                current_call = call;
            }
            if let Some(result) = patch.result {
                short_circuit = Some(result);
                break;
            }
        }
        (current_call, short_circuit)
    }

    pub fn apply_after_tool_call(
        &self,
        task: &AgentTask,
        cycle_index: u32,
        call: &ToolCall,
        context: &ToolContext,
        result: ToolExecutionResult,
    ) -> ToolExecutionResult {
        let mut current = result;
        for hook in &self.hooks {
            if let Some(patched) = hook.after_tool_call(AfterToolCallEvent {
                task,
                cycle_index,
                call,
                context,
                result: &current,
            }) {
                current = patched;
            }
        }
        current
    }
}
