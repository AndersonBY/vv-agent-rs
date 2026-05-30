use serde_json::Value;

use crate::types::{Message, ToolCall, ToolExecutionResult};

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

impl BeforeToolCallPatch {
    pub fn call(call: ToolCall) -> Self {
        Self {
            call: Some(call),
            result: None,
        }
    }

    pub fn result(result: ToolExecutionResult) -> Self {
        Self {
            call: None,
            result: Some(result),
        }
    }
}

impl From<ToolCall> for BeforeToolCallPatch {
    fn from(call: ToolCall) -> Self {
        Self::call(call)
    }
}

impl From<ToolExecutionResult> for BeforeToolCallPatch {
    fn from(result: ToolExecutionResult) -> Self {
        Self::result(result)
    }
}
