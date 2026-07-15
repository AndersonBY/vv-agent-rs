use crate::runtime::hooks::RuntimeHookManager;
use crate::tools::{ToolError, ToolOrchestrator, ToolRegistry, ToolRunOptions};
use crate::types::{CompletionReason, ToolDirective};

use super::outcome::ToolRunOutcome;
use super::request::ToolRunRequest;
use super::results::{
    apply_tool_use_behavior, image_notification_from_tool_result, needs_tool_call_id,
    skipped_tool_result,
};

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
        let mut completion_reason = None;
        let mut completion_tool_name = None;
        let mut interruption_messages = Vec::new();
        let mut image_notifications = Vec::new();
        let orchestrator = ToolOrchestrator::from_tools(self.tool_registry.executors());

        for (index, call) in request.tool_calls.iter().enumerate() {
            if let Some(context) = request.execution_context {
                context
                    .check_cancelled()
                    .map_err(|error| error.to_string())?;
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
                    let mut result = block_on_tool_run(orchestrator.run_one(
                        patched_call.clone(),
                        request.context,
                        ToolRunOptions::default(),
                    ))?;
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
            let behavior_reason = apply_tool_use_behavior(request.task, &patched_call, &mut result);

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
                completion_reason = behavior_reason.or(Some(match result.directive {
                    ToolDirective::WaitUser => CompletionReason::WaitUser,
                    ToolDirective::Finish => CompletionReason::ToolFinish,
                    ToolDirective::Continue => unreachable!(),
                }));
                completion_tool_name = Some(patched_call.name.clone());
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
            completion_reason,
            completion_tool_name,
            interruption_messages,
        })
    }
}

fn block_on_tool_run<'a>(
    future: impl std::future::Future<Output = Result<crate::types::ToolExecutionResult, ToolError>> + 'a,
) -> Result<crate::types::ToolExecutionResult, String> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
            tokio::task::block_in_place(|| handle.block_on(future))
                .map_err(|error| error.to_string())
        } else {
            handle.block_on(future).map_err(|error| error.to_string())
        }
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?
            .block_on(future)
            .map_err(|error| error.to_string())
    }
}
