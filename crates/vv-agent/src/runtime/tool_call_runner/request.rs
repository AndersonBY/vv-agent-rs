use crate::runtime::context::ExecutionContext;
use crate::tools::ToolContext;
use crate::types::{AgentTask, CycleRecord, Message, ToolCall, ToolExecutionResult};

pub type ToolResultCallback<'a> = dyn FnMut(&ToolCall, &ToolExecutionResult) + 'a;

pub struct ToolRunRequest<'a> {
    pub task: &'a AgentTask,
    pub tool_calls: Vec<ToolCall>,
    pub context: &'a mut ToolContext,
    pub messages: &'a mut Vec<Message>,
    pub cycle_record: &'a mut CycleRecord,
    pub interruption_provider: Option<&'a dyn Fn() -> Vec<Message>>,
    pub on_tool_result: Option<&'a mut ToolResultCallback<'a>>,
    pub execution_context: Option<&'a ExecutionContext>,
}

impl<'a> ToolRunRequest<'a> {
    pub fn new(
        task: &'a AgentTask,
        tool_calls: Vec<ToolCall>,
        context: &'a mut ToolContext,
        messages: &'a mut Vec<Message>,
        cycle_record: &'a mut CycleRecord,
    ) -> Self {
        Self {
            task,
            tool_calls,
            context,
            messages,
            cycle_record,
            interruption_provider: None,
            on_tool_result: None,
            execution_context: None,
        }
    }

    pub fn with_interruption_provider(mut self, provider: &'a dyn Fn() -> Vec<Message>) -> Self {
        self.interruption_provider = Some(provider);
        self
    }

    pub fn with_tool_result_callback(mut self, callback: &'a mut ToolResultCallback<'a>) -> Self {
        self.on_tool_result = Some(callback);
        self
    }

    pub fn with_execution_context(mut self, execution_context: &'a ExecutionContext) -> Self {
        self.execution_context = Some(execution_context);
        self
    }
}
