use std::collections::BTreeMap;

use serde_json::Value;

use crate::tools::ToolContext;
use crate::types::{AgentTask, LLMResponse, Message, ToolCall, ToolExecutionResult};

pub struct BeforeMemoryCompactEvent<'a> {
    pub task: &'a AgentTask,
    pub cycle_index: u32,
    pub messages: &'a [Message],
    pub shared_state: &'a BTreeMap<String, Value>,
}

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
