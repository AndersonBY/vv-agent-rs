use crate::types::{Message, ToolExecutionResult};

pub struct ToolRunOutcome {
    pub directive_result: Option<ToolExecutionResult>,
    pub interruption_messages: Vec<Message>,
}
