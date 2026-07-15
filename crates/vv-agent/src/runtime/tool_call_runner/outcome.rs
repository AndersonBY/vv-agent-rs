use crate::types::{CompletionReason, Message, ToolExecutionResult};

pub struct ToolRunOutcome {
    pub directive_result: Option<ToolExecutionResult>,
    pub completion_reason: Option<CompletionReason>,
    pub completion_tool_name: Option<String>,
    pub interruption_messages: Vec<Message>,
}
