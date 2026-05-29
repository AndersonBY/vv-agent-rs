use std::collections::BTreeMap;

use super::context::ExecutionContext;
use super::hooks::RuntimeHookManager;
use crate::tools::{ToolContext, ToolRegistry};
use crate::types::{
    AgentTask, CycleRecord, Message, ToolCall, ToolDirective, ToolExecutionResult, ToolResultStatus,
};

pub type ToolResultCallback<'a> = dyn FnMut(&ToolCall, &ToolExecutionResult) + 'a;

pub struct ToolRunOutcome {
    pub directive_result: Option<ToolExecutionResult>,
    pub interruption_messages: Vec<Message>,
}

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

pub struct ToolCallRunner {
    tool_registry: ToolRegistry,
    hook_manager: RuntimeHookManager,
}

impl ToolCallRunner {
    pub fn new(tool_registry: ToolRegistry) -> Self {
        Self {
            tool_registry,
            hook_manager: RuntimeHookManager::default(),
        }
    }

    pub fn with_hook_manager(mut self, hook_manager: RuntimeHookManager) -> Self {
        self.hook_manager = hook_manager;
        self
    }

    pub fn run(&self, mut request: ToolRunRequest<'_>) -> Result<ToolRunOutcome, String> {
        let mut directive_result = None;
        let mut interruption_messages = Vec::new();
        let mut image_notifications = Vec::new();

        for (index, call) in request.tool_calls.iter().enumerate() {
            if let Some(context) = request.execution_context {
                context.check_cancelled()?;
            }
            let (patched_call, short_circuit_result) = self.hook_manager.apply_before_tool_call(
                request.task,
                request.context.cycle_index,
                call.clone(),
                request.context,
            );
            let mut result = match short_circuit_result {
                Some(mut result) => {
                    if needs_tool_call_id(&result.tool_call_id) {
                        result.tool_call_id = call.id.clone();
                    }
                    result
                }
                None => {
                    let mut result =
                        execute_tool_result(&self.tool_registry, &patched_call, request.context);
                    if needs_tool_call_id(&result.tool_call_id) {
                        result.tool_call_id = patched_call.id.clone();
                    }
                    result
                }
            };
            result = self.hook_manager.apply_after_tool_call(
                request.task,
                request.context.cycle_index,
                &patched_call,
                request.context,
                result,
            );
            if needs_tool_call_id(&result.tool_call_id) {
                result.tool_call_id = patched_call.id.clone();
            }

            request.messages.push(result.to_message());
            if let Some(image_notification) =
                image_notification_from_tool_result(&result, request.task.native_multimodal)
            {
                image_notifications.push(image_notification);
            }
            request.cycle_record.tool_results.push(result.clone());
            if let Some(callback) = request.on_tool_result.as_deref_mut() {
                callback(call, &result);
            }

            if result.directive != ToolDirective::Continue {
                directive_result = Some(result.clone());
                let (error_code, message) = match result.directive {
                    ToolDirective::WaitUser => (
                        "skipped_due_to_wait_user",
                        "Tool skipped because a previous tool requested user input.",
                    ),
                    ToolDirective::Finish => (
                        "skipped_due_to_finish",
                        "Tool skipped because a previous tool finished the task.",
                    ),
                    ToolDirective::Continue => ("skipped_due_to_directive", "Tool skipped."),
                };
                for skipped_call in request.tool_calls.iter().skip(index + 1) {
                    let skipped = skipped_tool_result(skipped_call, error_code, message);
                    request.messages.push(skipped.to_message());
                    request.cycle_record.tool_results.push(skipped.clone());
                    if let Some(callback) = request.on_tool_result.as_deref_mut() {
                        callback(skipped_call, &skipped);
                    }
                }
                break;
            }

            if let Some(provider) = request.interruption_provider {
                let pending_messages = provider();
                if !pending_messages.is_empty() {
                    interruption_messages.extend(pending_messages);
                    for skipped_call in request.tool_calls.iter().skip(index + 1) {
                        let skipped = skipped_tool_result(
                            skipped_call,
                            "skipped_due_to_steering",
                            "Tool skipped due to queued steering message.",
                        );
                        request.messages.push(skipped.to_message());
                        request.cycle_record.tool_results.push(skipped.clone());
                        if let Some(callback) = request.on_tool_result.as_deref_mut() {
                            callback(skipped_call, &skipped);
                        }
                    }
                    break;
                }
            }
        }

        request.messages.extend(image_notifications);
        Ok(ToolRunOutcome {
            directive_result,
            interruption_messages,
        })
    }
}

pub(crate) fn execute_tool_result(
    registry: &ToolRegistry,
    call: &ToolCall,
    context: &mut ToolContext,
) -> ToolExecutionResult {
    crate::tools::dispatch_tool_call(registry, context, call)
}

pub(crate) fn needs_tool_call_id(value: &str) -> bool {
    let stripped = value.trim();
    stripped.is_empty() || stripped == "pending"
}

pub(crate) fn skipped_tool_result(
    call: &ToolCall,
    error_code: &str,
    message: &str,
) -> ToolExecutionResult {
    ToolExecutionResult {
        tool_call_id: call.id.clone(),
        content: serde_json::json!({
            "ok": false,
            "error": message,
            "error_code": error_code,
        })
        .to_string(),
        status: ToolResultStatus::Error,
        directive: ToolDirective::Continue,
        error_code: Some(error_code.to_string()),
        metadata: BTreeMap::new(),
        image_url: None,
        image_path: None,
    }
}

fn image_notification_from_tool_result(
    result: &ToolExecutionResult,
    include_image: bool,
) -> Option<Message> {
    if !include_image {
        return None;
    }
    if let Some(image_url) = &result.image_url {
        let content = result
            .image_path
            .as_deref()
            .map(|image_path| format!("[Image loaded] {image_path}"))
            .unwrap_or_default();
        let mut image_message = Message::user(content);
        image_message.image_url = Some(image_url.clone());
        image_message.metadata = result.metadata.clone();
        return Some(image_message);
    }
    result
        .image_path
        .as_deref()
        .map(|image_path| Message::user(format!("[Image loaded] {image_path}")))
}
