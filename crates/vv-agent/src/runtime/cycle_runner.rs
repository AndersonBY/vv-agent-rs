use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::context::ExecutionContext;
use super::hooks::RuntimeHookManager;
use super::model_calls::{ModelCallCoordinator, ModelCallLedger};
use super::results::assistant_message_from_response;
use crate::llm::{LlmClient, LlmError, LlmRequest};
use crate::memory::{CompactionExhaustedError, MemoryManager};
use crate::tools::ToolRegistry;
use crate::types::{AgentTask, CycleRecord, Message, ModelCallOperation};

pub const MAX_PROMPT_TOO_LONG_RETRIES: u32 = 3;

const PROMPT_TOO_LONG_PATTERNS: &[&str] = &[
    "prompt is too long",
    "prompt_too_long",
    "context_length_exceeded",
    "maximum context length",
    "request too large",
    "too many tokens",
];

pub fn is_prompt_too_long_error(error: &LlmError) -> bool {
    let text = error.to_string().to_ascii_lowercase();
    PROMPT_TOO_LONG_PATTERNS
        .iter()
        .any(|pattern| text.contains(pattern))
}

pub struct CycleRunRequest<'a> {
    pub task: &'a AgentTask,
    pub messages: Vec<Message>,
    pub cycle_index: u32,
    pub memory_manager: &'a mut MemoryManager,
    pub previous_prompt_tokens: Option<u64>,
    pub recent_tool_call_ids: Option<&'a BTreeSet<String>>,
    pub shared_state: Option<&'a BTreeMap<String, Value>>,
    pub execution_context: Option<&'a ExecutionContext>,
}

impl<'a> CycleRunRequest<'a> {
    pub fn new(
        task: &'a AgentTask,
        messages: Vec<Message>,
        cycle_index: u32,
        memory_manager: &'a mut MemoryManager,
    ) -> Self {
        Self {
            task,
            messages,
            cycle_index,
            memory_manager,
            previous_prompt_tokens: None,
            recent_tool_call_ids: None,
            shared_state: None,
            execution_context: None,
        }
    }

    pub fn with_previous_prompt_tokens(mut self, previous_prompt_tokens: Option<u64>) -> Self {
        self.previous_prompt_tokens = previous_prompt_tokens;
        self
    }

    pub fn with_recent_tool_call_ids(mut self, recent_tool_call_ids: &'a BTreeSet<String>) -> Self {
        self.recent_tool_call_ids = Some(recent_tool_call_ids);
        self
    }

    pub fn with_shared_state(mut self, shared_state: &'a BTreeMap<String, Value>) -> Self {
        self.shared_state = Some(shared_state);
        self
    }

    pub fn with_execution_context(mut self, execution_context: &'a ExecutionContext) -> Self {
        self.execution_context = Some(execution_context);
        self
    }
}

pub struct CycleRunner<C: LlmClient> {
    llm_client: C,
    tool_registry: ToolRegistry,
    hook_manager: RuntimeHookManager,
}

impl<C: LlmClient> CycleRunner<C> {
    pub fn new(llm_client: C, tool_registry: ToolRegistry) -> Self {
        Self {
            llm_client,
            tool_registry,
            hook_manager: RuntimeHookManager::default(),
        }
    }

    pub fn with_hook_manager(mut self, hook_manager: RuntimeHookManager) -> Self {
        self.hook_manager = hook_manager;
        self
    }

    pub fn run_cycle(
        &self,
        request: CycleRunRequest<'_>,
    ) -> Result<(Vec<Message>, CycleRecord), LlmError> {
        if let Some(context) = request.execution_context {
            check_context_cancelled(context)?;
        }
        let empty_shared_state = BTreeMap::new();
        let shared_state = request.shared_state.unwrap_or(&empty_shared_state);
        let pre_compact_messages = self.hook_manager.apply_before_memory_compact(
            request.task,
            request.cycle_index,
            request.messages,
            shared_state,
        );
        let (mut compacted_messages, mut memory_compacted) =
            request.memory_manager.compact_for_cycle_with_usage(
                &pre_compact_messages,
                request.cycle_index,
                false,
                request.previous_prompt_tokens,
                request.recent_tool_call_ids,
            );

        let mut prompt_too_long_retries = 0;
        let coordinator = request
            .execution_context
            .and_then(|context| context.runtime_state.model_call_coordinator.clone())
            .unwrap_or_else(|| {
                let event_handler = request
                    .execution_context
                    .and_then(|context| context.event_handler.clone());
                ModelCallCoordinator::new(
                    ModelCallLedger::default(),
                    &request.task.task_id,
                    &request.task.task_id,
                    &request.task.task_id,
                    None,
                    None,
                    event_handler,
                    None,
                )
            });
        let (response, request_messages, request_tool_schemas) = loop {
            let llm_messages = request
                .memory_manager
                .apply_session_memory_context(&compacted_messages);
            let tool_schemas = self.tool_registry.planned_openai_schemas(request.task);
            let (request_messages, request_tool_schemas) = self.hook_manager.apply_before_llm(
                request.task,
                request.cycle_index,
                llm_messages,
                tool_schemas,
                shared_state,
            );
            if let Some(context) = request.execution_context {
                check_context_cancelled(context)?;
            }
            let mut llm_request =
                LlmRequest::new(request.task.model.clone(), request_messages.clone());
            llm_request.tools = request_tool_schemas.clone();
            llm_request.metadata =
                Value::Object(request.task.metadata.clone().into_iter().collect());
            llm_request.model_settings = request.task.model_settings.clone();
            let backend = request
                .execution_context
                .and_then(|context| {
                    context
                        .metadata
                        .get("_vv_agent_resolved_backend")
                        .and_then(Value::as_str)
                })
                .unwrap_or("direct");
            let model = request
                .execution_context
                .and_then(|context| {
                    context
                        .metadata
                        .get("_vv_agent_resolved_model")
                        .and_then(Value::as_str)
                })
                .unwrap_or(&request.task.model);
            let operation_slot = if prompt_too_long_retries == 0 {
                "main".to_string()
            } else {
                format!("prompt_too_long_{prompt_too_long_retries}")
            };
            match coordinator.dispatch(
                ModelCallOperation::AgentCycle,
                request.cycle_index,
                &operation_slot,
                backend,
                model,
                &llm_request,
                || self.llm_client.complete(llm_request.clone()),
            ) {
                Ok(dispatch) => break (dispatch.response, request_messages, request_tool_schemas),
                Err(error) if is_prompt_too_long_error(&error) => {
                    prompt_too_long_retries += 1;
                    if prompt_too_long_retries > MAX_PROMPT_TOO_LONG_RETRIES {
                        return Err(LlmError::CompactionExhausted(
                            CompactionExhaustedError::new(
                                prompt_too_long_retries,
                                Some(error.to_string()),
                            ),
                        ));
                    }
                    if prompt_too_long_retries == 1 {
                        (compacted_messages, _) =
                            request.memory_manager.compact_for_cycle_with_usage(
                                &compacted_messages,
                                request.cycle_index,
                                true,
                                None,
                                request.recent_tool_call_ids,
                            );
                    } else {
                        compacted_messages = request.memory_manager.emergency_compact(
                            &compacted_messages,
                            (0.2 * f64::from(prompt_too_long_retries)).min(0.95),
                        );
                    }
                    memory_compacted = true;
                }
                Err(error) => return Err(error),
            }
        };

        if let Some(context) = request.execution_context {
            check_context_cancelled(context)?;
        }
        let response = self.hook_manager.apply_after_llm(
            request.task,
            request.cycle_index,
            &request_messages,
            &request_tool_schemas,
            response,
            shared_state,
        );
        let mut next_messages = request_messages;
        next_messages.push(assistant_message_from_response(&response));
        let mut cycle = CycleRecord::from_response(request.cycle_index, &response, Vec::new());
        cycle.memory_compacted = memory_compacted;
        Ok((next_messages, cycle))
    }
}

fn check_context_cancelled(context: &ExecutionContext) -> Result<(), LlmError> {
    context
        .check_cancelled()
        .map_err(|error| LlmError::Request(error.to_string()))
}
