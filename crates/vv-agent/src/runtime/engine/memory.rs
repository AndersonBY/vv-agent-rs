use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::events::{MemoryCompactMode, MemoryCompactTrigger, ReservedOutputSource, RunEvent};
use crate::llm::LlmClient;
use crate::memory::token_utils::count_messages_tokens;
use crate::memory::{
    provider::block_on_memory_future, MemoryManager, MemoryManagerConfig, MemoryProvider,
    RuntimeMemoryCallbackError,
};
use crate::runtime::context::ExecutionContext;
use crate::runtime::hooks::RuntimeHookManager;
use crate::runtime::model_calls::ModelCallLedger;
use crate::tools::ToolRegistry;
use crate::types::{AgentResult, AgentTask, CycleRecord, Message, Metadata};
use crate::workspace::WorkspaceBackend;

use super::budget::{budget_failure_result, lock_budget, SharedRunBudgetController};
use super::checkpoint::CheckpointCoordinator;
use super::helpers::{
    cancelled_agent_result, failed_agent_result, previous_cycle_memory_usage, task_token_usage,
};
use super::{AgentRuntime, RuntimeRunControls};

mod callbacks;
mod metadata;
mod session;
mod token_limits;

pub(super) use callbacks::{
    build_runtime_memory_callbacks, decode_control as decode_memory_inference_control,
    MemoryInferenceControl,
};
use metadata::{
    read_bool_metadata, read_optional_string_metadata, read_optional_u64_metadata,
    read_string_metadata, read_u64_metadata, read_usize_metadata,
};
use session::build_session_memory;
use token_limits::resolve_runtime_model_token_limits;

const DEFAULT_MEMORY_COMPACT_THRESHOLD: u64 = 250_000;
const RESERVED_OUTPUT_TOKENS_FALLBACK: u64 = 16_000;
const AUTOCOMPACT_BUFFER_TOKENS_DEFAULT: u64 = 13_000;

#[derive(Debug, Clone, Copy)]
struct RuntimeMemoryCapacity {
    model_context_window: u64,
    model_max_output_tokens: Option<u64>,
    reserved_output_tokens: u64,
    reserved_output_source: ReservedOutputSource,
}

pub(super) struct CycleMemoryCompaction {
    pub messages: Vec<Message>,
    pub changed: bool,
    pub recent_tool_call_ids: Option<BTreeSet<String>>,
}

pub(super) struct MemoryCompactCompletion<'a> {
    cycle_index: u32,
    before_messages: &'a [Message],
    after_messages: &'a [Message],
    model: &'a str,
    mode: MemoryCompactMode,
    archived_count: usize,
    reclaimed_tokens: u64,
    artifact_failure_count: usize,
}

impl<'a> MemoryCompactCompletion<'a> {
    pub(super) fn new(
        cycle_index: u32,
        before_messages: &'a [Message],
        after_messages: &'a [Message],
        model: &'a str,
        mode: MemoryCompactMode,
    ) -> Self {
        Self {
            cycle_index,
            before_messages,
            after_messages,
            model,
            mode,
            archived_count: 0,
            reclaimed_tokens: 0,
            artifact_failure_count: 0,
        }
    }

    pub(super) fn with_archive_stats(
        mut self,
        archived_count: usize,
        reclaimed_tokens: u64,
        artifact_failure_count: usize,
    ) -> Self {
        self.archived_count = archived_count;
        self.reclaimed_tokens = reclaimed_tokens;
        self.artifact_failure_count = artifact_failure_count;
        self
    }
}

pub(super) fn build_memory_manager(
    task: &AgentTask,
    workspace_path: PathBuf,
    workspace_backend: Arc<dyn WorkspaceBackend>,
    tool_registry: &ToolRegistry,
    settings_file: Option<&Path>,
    default_backend: Option<&str>,
) -> Result<MemoryManager, String> {
    task.microcompaction_policy
        .validate()
        .map_err(|error| error.to_string())?;
    let workspace = task.use_workspace.then_some(workspace_path);
    let summary_backend =
        read_optional_string_metadata(&task.metadata, &["memory_summary_backend"])
            .or_else(|| default_backend.map(str::to_string));
    let summary_model = read_optional_string_metadata(&task.metadata, &["memory_summary_model"])
        .unwrap_or_else(|| task.model.clone());
    let (resolved_context_window, resolved_max_output_tokens) =
        resolve_runtime_model_token_limits(settings_file, default_backend, &task.model);
    let autocompact_buffer_tokens = read_u64_metadata(
        &task.metadata,
        "autocompact_buffer_tokens",
        AUTOCOMPACT_BUFFER_TOKENS_DEFAULT,
    );
    let capacity = resolve_memory_capacity(
        task,
        resolved_context_window,
        resolved_max_output_tokens,
        autocompact_buffer_tokens,
    );

    let session_memory = build_session_memory(
        task,
        workspace.clone(),
        None,
        summary_backend.clone(),
        summary_model.clone(),
    )?;
    let recovery_tool_available = tool_registry
        .planned_openai_schemas(task)
        .iter()
        .any(|schema| {
            schema["function"]["name"].as_str() == Some(crate::constants::READ_FILE_TOOL_NAME)
        });
    Ok(MemoryManager::new(MemoryManagerConfig {
        compact_threshold: task.memory_compact_threshold,
        keep_recent_messages: read_usize_metadata(
            &task.metadata,
            "memory_keep_recent_messages",
            10,
        ),
        model: task.model.clone(),
        model_context_window: capacity.model_context_window,
        reserved_output_tokens: capacity.reserved_output_tokens,
        autocompact_buffer_tokens,
        language: read_string_metadata(&task.metadata, "language", "zh-CN"),
        warning_threshold_percentage: task.memory_threshold_percentage.clamp(1, 100),
        include_memory_warning: read_bool_metadata(&task.metadata, "include_memory_warning", false),
        summary_event_limit: read_usize_metadata(&task.metadata, "summary_event_limit", 40),
        summary_backend: summary_backend.clone(),
        summary_model: Some(summary_model.clone()),
        summary_callback: None,
        tool_result_compact_threshold: read_usize_metadata(
            &task.metadata,
            "tool_result_compact_threshold",
            2_000,
        ),
        tool_result_keep_last: read_usize_metadata(&task.metadata, "tool_result_keep_last", 3),
        tool_result_excerpt_head: read_usize_metadata(
            &task.metadata,
            "tool_result_excerpt_head",
            200,
        ),
        tool_result_excerpt_tail: read_usize_metadata(
            &task.metadata,
            "tool_result_excerpt_tail",
            200,
        ),
        tool_calls_keep_last: read_usize_metadata(&task.metadata, "tool_calls_keep_last", 3),
        assistant_no_tool_keep_last: read_usize_metadata(
            &task.metadata,
            "assistant_no_tool_keep_last",
            1,
        ),
        microcompaction_policy: task.microcompaction_policy,
        workspace: workspace.clone(),
        session_memory,
    })
    .with_workspace_backend(workspace_backend)
    .with_archive_context(
        task.task_id.clone(),
        tool_registry.result_retentions(),
        recovery_tool_available,
    )
    .with_capacity_observation(
        capacity.model_max_output_tokens,
        capacity.reserved_output_source,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn compact_cycle_memory<C>(
    runtime: &AgentRuntime<C>,
    controls: &RuntimeRunControls,
    task: &AgentTask,
    hook_manager: &RuntimeHookManager,
    memory_manager: &mut MemoryManager,
    cycle_index: u32,
    messages: &[Message],
    cycles: &[CycleRecord],
    shared_state: &Metadata,
    model_call_ledger: &ModelCallLedger,
) -> Result<CycleMemoryCompaction, RuntimeMemoryCallbackError>
where
    C: LlmClient + Clone + 'static,
{
    let pre_compact_messages = hook_manager.apply_before_memory_compact(
        task,
        cycle_index,
        messages.to_vec(),
        shared_state,
    );
    let (previous_prompt_tokens, recent_tool_call_ids) = previous_cycle_memory_usage(
        cycles,
        model_call_ledger.previous_agent_input_tokens(cycle_index),
    );
    let effective_usage = memory_manager.effective_length_for_cycle(
        &pre_compact_messages,
        previous_prompt_tokens,
        recent_tool_call_ids.as_ref(),
    );
    let microcompaction_plan = memory_manager.plan_cycle_microcompaction(
        &pre_compact_messages,
        cycle_index,
        effective_usage,
    );
    let memory_compact_event = memory_compact_started_event(
        controls.execution_context.as_ref(),
        memory_manager,
        task,
        cycle_index,
        &pre_compact_messages,
        previous_prompt_tokens,
        recent_tool_call_ids.as_ref(),
        false,
        microcompaction_plan.candidate_count,
        microcompaction_plan.estimated_reclaimable_tokens,
    )
    .map(|event| {
        let event = notify_memory_before_compact(
            controls.execution_context.as_ref(),
            event,
            &pre_compact_messages,
        );
        runtime.emit_log(
            controls,
            "memory_compact_started",
            memory_compact_event_payload(&event),
        );
        event
    });
    let compaction_outcome = memory_manager.compact_for_cycle_with_usage_observed(
        &pre_compact_messages,
        cycle_index,
        false,
        previous_prompt_tokens,
        recent_tool_call_ids.as_ref(),
        Some(microcompaction_plan),
    )?;
    if let Some(started_event) = memory_compact_event.as_ref() {
        let completed = memory_compact_completed_event(
            started_event,
            MemoryCompactCompletion::new(
                cycle_index,
                &pre_compact_messages,
                &compaction_outcome.messages,
                &memory_manager.config.model,
                compaction_outcome.mode,
            )
            .with_archive_stats(
                compaction_outcome.archived_count,
                compaction_outcome.reclaimed_tokens,
                compaction_outcome.artifact_failure_count,
            ),
        );
        let completed = notify_memory_after_compact(controls.execution_context.as_ref(), completed);
        runtime.emit_log(
            controls,
            "memory_compact_completed",
            memory_compact_event_payload(&completed),
        );
    }
    Ok(CycleMemoryCompaction {
        messages: compaction_outcome.messages,
        changed: compaction_outcome.changed,
        recent_tool_call_ids,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn memory_inference_failure_result(
    error: RuntimeMemoryCallbackError,
    checkpoint: &CheckpointCoordinator,
    budget_controller: &Option<SharedRunBudgetController>,
    controls: &RuntimeRunControls,
    messages: &[Message],
    cycles: &[CycleRecord],
    shared_state: &Metadata,
) -> AgentResult {
    match decode_memory_inference_control(error) {
        Ok(MemoryInferenceControl::BudgetExhausted(exhaustion)) => {
            let controller = budget_controller
                .as_ref()
                .expect("memory model-call exhaustion requires a budget controller");
            let controller = lock_budget(controller);
            budget_failure_result(
                messages.to_vec(),
                cycles.to_vec(),
                shared_state.clone(),
                &controller,
                exhaustion,
                task_token_usage(controls),
            )
        }
        Ok(MemoryInferenceControl::Cancelled) => cancelled_agent_result(
            messages.to_vec(),
            cycles.to_vec(),
            shared_state.clone(),
            task_token_usage(controls),
        ),
        Ok(MemoryInferenceControl::Interrupted(result)) => *result,
        Ok(MemoryInferenceControl::CheckpointFailed(error)) => {
            checkpoint.failure_result(error, messages, cycles, shared_state)
        }
        Err(_) => failed_agent_result(
            messages.to_vec(),
            cycles.to_vec(),
            shared_state.clone(),
            "memory inference callback failed".to_string(),
            task_token_usage(controls),
        ),
    }
}

fn resolve_memory_capacity(
    task: &AgentTask,
    resolved_context_window: Option<u64>,
    resolved_max_output_tokens: Option<u64>,
    autocompact_buffer_tokens: u64,
) -> RuntimeMemoryCapacity {
    let declared_context_window =
        read_optional_u64_metadata(&task.metadata, "model_context_window")
            .filter(|value| *value > 0)
            .or(resolved_context_window.filter(|value| *value > 0));
    let model_max_output_tokens =
        read_optional_u64_metadata(&task.metadata, "model_max_output_tokens")
            .or(resolved_max_output_tokens);

    let request_limit = task
        .model_settings
        .as_ref()
        .and_then(|settings| settings.max_tokens)
        .filter(|limit| *limit > 0)
        .map(u64::from);
    let explicit_host_reserve =
        read_optional_u64_metadata(&task.metadata, "reserved_output_tokens");
    let (reserved_output_tokens, reserved_output_source) = if let Some(limit) = request_limit {
        (limit, ReservedOutputSource::ModelSettings)
    } else if let Some(limit) = explicit_host_reserve {
        (limit, ReservedOutputSource::TaskMetadata)
    } else if let Some(capability) =
        model_max_output_tokens.filter(|capability| *capability < RESERVED_OUTPUT_TOKENS_FALLBACK)
    {
        (
            capability,
            ReservedOutputSource::FrameworkFallbackCappedByModelCapability,
        )
    } else {
        (
            RESERVED_OUTPUT_TOKENS_FALLBACK,
            ReservedOutputSource::FrameworkFallback,
        )
    };
    let planning_prompt_capacity = if task.memory_compact_threshold > 0 {
        task.memory_compact_threshold
    } else {
        DEFAULT_MEMORY_COMPACT_THRESHOLD
    };
    let model_context_window = declared_context_window.unwrap_or_else(|| {
        planning_prompt_capacity
            .saturating_add(reserved_output_tokens)
            .saturating_add(autocompact_buffer_tokens)
    });

    RuntimeMemoryCapacity {
        model_context_window,
        model_max_output_tokens,
        reserved_output_tokens,
        reserved_output_source,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn memory_compact_started_event(
    execution_context: Option<&ExecutionContext>,
    memory_manager: &MemoryManager,
    task: &AgentTask,
    cycle_index: u32,
    messages: &[Message],
    previous_prompt_tokens: Option<u64>,
    recent_tool_call_ids: Option<&BTreeSet<String>>,
    force: bool,
    candidate_count: usize,
    estimated_reclaimable_tokens: u64,
) -> Option<RunEvent> {
    let trigger = if force {
        MemoryCompactTrigger::PromptTooLong
    } else {
        memory_manager.compaction_trigger(messages, previous_prompt_tokens, recent_tool_call_ids)?
    };
    if trigger == MemoryCompactTrigger::MicroThreshold && candidate_count == 0 {
        return None;
    }
    let identity = execution_context.map(|context| &context.metadata);
    let run_id = identity
        .and_then(|metadata| metadata.get("_vv_agent_run_id"))
        .and_then(Value::as_str)
        .unwrap_or(&task.task_id)
        .to_string();
    let trace_id = identity
        .and_then(|metadata| metadata.get("_vv_agent_trace_id"))
        .and_then(Value::as_str)
        .or_else(|| task.metadata.get("trace_id").and_then(Value::as_str))
        .unwrap_or(&run_id)
        .to_string();
    let agent_name = identity
        .and_then(|metadata| metadata.get("_vv_agent_agent_name"))
        .or_else(|| task.metadata.get("agent_name"))
        .and_then(Value::as_str)
        .unwrap_or(&task.task_id)
        .to_string();
    let event = RunEvent::memory_compact_started(
        run_id,
        trace_id,
        agent_name,
        cycle_index,
        messages.len(),
        previous_prompt_tokens.or_else(|| {
            Some(count_messages_tokens(
                messages,
                &memory_manager.config.model,
            ))
        }),
        trigger,
        memory_manager.config.compact_threshold,
        memory_manager.autocompact_threshold(),
        memory_manager.microcompact_trigger_threshold(),
        memory_manager.microcompact_target_threshold(),
        candidate_count,
        estimated_reclaimable_tokens,
        memory_manager.config.model_context_window,
        memory_manager.model_max_output_tokens(),
        memory_manager.config.reserved_output_tokens,
        memory_manager.reserved_output_source(),
        memory_manager.config.autocompact_buffer_tokens,
    );
    Some(
        match identity
            .and_then(|metadata| metadata.get("_vv_agent_session_id"))
            .and_then(Value::as_str)
        {
            Some(session_id) => event.with_session_id(session_id),
            None => event,
        },
    )
}

pub(super) fn notify_memory_before_compact(
    execution_context: Option<&ExecutionContext>,
    mut event: RunEvent,
    messages: &[Message],
) -> RunEvent {
    let provider_event = event.clone().with_metadata(
        "messages",
        serde_json::to_value(messages).unwrap_or(Value::Null),
    );
    let mut results = BTreeMap::new();
    let mut errors = Vec::new();
    let mut seen_names = BTreeMap::new();
    for (index, provider) in memory_providers(execution_context).into_iter().enumerate() {
        let provider_name = memory_provider_name(provider, index, &mut seen_names);
        match block_on_memory_future(provider.before_compact(&provider_event)) {
            Ok(result) if !result.metadata.is_empty() => {
                results.insert(
                    provider_name,
                    Value::Object(result.metadata.into_iter().collect()),
                );
            }
            Ok(_) => {}
            Err(error) => errors.push(memory_provider_error(
                provider_name,
                "before_compact",
                error,
            )),
        }
    }
    if !results.is_empty() {
        event = event.with_metadata(
            "memory_provider_results",
            Value::Object(results.into_iter().collect()),
        );
    }
    if !errors.is_empty() {
        event = event.with_metadata("memory_provider_errors", Value::Array(errors));
    }
    event
}

pub(super) fn notify_memory_after_compact(
    execution_context: Option<&ExecutionContext>,
    mut event: RunEvent,
) -> RunEvent {
    let mut errors = Vec::new();
    let mut seen_names = BTreeMap::new();
    for (index, provider) in memory_providers(execution_context).into_iter().enumerate() {
        let provider_name = memory_provider_name(provider, index, &mut seen_names);
        if let Err(error) = block_on_memory_future(provider.after_compact(&event)) {
            errors.push(memory_provider_error(provider_name, "after_compact", error));
        }
    }
    if !errors.is_empty() {
        event = event.with_metadata("memory_provider_errors", Value::Array(errors));
    }
    event
}

fn memory_providers(execution_context: Option<&ExecutionContext>) -> Vec<&Arc<dyn MemoryProvider>> {
    execution_context
        .map(|context| context.memory_providers.iter().collect())
        .unwrap_or_default()
}

pub(super) fn memory_compact_completed_event(
    started_event: &RunEvent,
    completion: MemoryCompactCompletion<'_>,
) -> RunEvent {
    let event = RunEvent::memory_compact_completed(
        started_event.run_id(),
        started_event.trace_id(),
        started_event
            .agent_name()
            .expect("memory compact event has agent identity"),
        completion.cycle_index,
        completion.before_messages.len(),
        completion.after_messages.len(),
        Some(count_messages_tokens(
            completion.after_messages,
            completion.model,
        )),
        completion.mode,
        completion.before_messages != completion.after_messages,
        completion.archived_count,
        completion.reclaimed_tokens,
        completion.artifact_failure_count,
    );
    match started_event.session_id() {
        Some(session_id) => event.with_session_id(session_id),
        None => event,
    }
}

pub(super) fn memory_compact_event_payload(event: &RunEvent) -> BTreeMap<String, Value> {
    let mut payload = event.metadata().clone();
    payload.insert(
        "event_id".to_string(),
        Value::String(event.event_id().as_str().to_string()),
    );
    payload.insert("created_at".to_string(), Value::from(event.created_at()));
    if let Some(cycle_index) = event.cycle_index() {
        payload.insert("cycle".to_string(), Value::from(cycle_index));
    }
    match event.payload() {
        crate::events::RunEventPayload::MemoryCompactStarted {
            message_count,
            estimated_tokens,
            trigger,
            configured_threshold,
            effective_threshold,
            microcompact_threshold,
            microcompact_target,
            candidate_count,
            estimated_reclaimable_tokens,
            model_context_window,
            model_max_output_tokens,
            reserved_output_tokens,
            reserved_output_source,
            autocompact_buffer_tokens,
        } => {
            payload.insert("message_count".to_string(), Value::from(*message_count));
            if let Some(estimated_tokens) = estimated_tokens {
                payload.insert(
                    "estimated_tokens".to_string(),
                    Value::from(*estimated_tokens),
                );
            }
            insert_serializable(&mut payload, "trigger", trigger);
            insert_serializable(&mut payload, "configured_threshold", configured_threshold);
            insert_serializable(&mut payload, "effective_threshold", effective_threshold);
            insert_serializable(
                &mut payload,
                "microcompact_threshold",
                microcompact_threshold,
            );
            insert_serializable(&mut payload, "microcompact_target", microcompact_target);
            insert_serializable(&mut payload, "candidate_count", candidate_count);
            insert_serializable(
                &mut payload,
                "estimated_reclaimable_tokens",
                estimated_reclaimable_tokens,
            );
            insert_serializable(&mut payload, "model_context_window", model_context_window);
            payload.insert(
                "model_max_output_tokens".to_string(),
                model_max_output_tokens
                    .map(Value::from)
                    .unwrap_or(Value::Null),
            );
            insert_serializable(
                &mut payload,
                "reserved_output_tokens",
                reserved_output_tokens,
            );
            insert_serializable(
                &mut payload,
                "reserved_output_source",
                reserved_output_source,
            );
            insert_serializable(
                &mut payload,
                "autocompact_buffer_tokens",
                autocompact_buffer_tokens,
            );
        }
        crate::events::RunEventPayload::MemoryCompactCompleted {
            before_count,
            after_count,
            summary_tokens,
            mode,
            changed,
            archived_count,
            reclaimed_tokens,
            artifact_failure_count,
        } => {
            payload.insert("before_count".to_string(), Value::from(*before_count));
            payload.insert("after_count".to_string(), Value::from(*after_count));
            if let Some(summary_tokens) = summary_tokens {
                payload.insert("summary_tokens".to_string(), Value::from(*summary_tokens));
            }
            insert_serializable(&mut payload, "mode", mode);
            insert_serializable(&mut payload, "changed", changed);
            insert_serializable(&mut payload, "archived_count", archived_count);
            insert_serializable(&mut payload, "reclaimed_tokens", reclaimed_tokens);
            insert_serializable(
                &mut payload,
                "artifact_failure_count",
                artifact_failure_count,
            );
        }
        _ => {}
    }
    payload
}

fn insert_serializable<T: serde::Serialize>(
    payload: &mut BTreeMap<String, Value>,
    key: &str,
    value: &T,
) {
    payload.insert(
        key.to_string(),
        serde_json::to_value(value).unwrap_or(Value::Null),
    );
}

fn memory_provider_name(
    provider: &Arc<dyn MemoryProvider>,
    index: usize,
    seen_names: &mut BTreeMap<String, usize>,
) -> String {
    let base_name = provider
        .provider_name()
        .rsplit("::")
        .next()
        .unwrap_or("MemoryProvider")
        .to_string();
    let seen = seen_names.entry(base_name.clone()).or_insert(0);
    let name = if *seen == 0 {
        base_name
    } else {
        format!("{base_name}#{}", index + 1)
    };
    *seen += 1;
    name
}

fn memory_provider_error(
    provider_name: String,
    stage: &str,
    error: crate::memory::MemoryError,
) -> Value {
    eprintln!("warning: Memory provider {provider_name} {stage} failed: {error}");
    serde_json::json!({
        "provider": provider_name,
        "stage": stage,
        "error": error.to_string(),
        "error_type": "MemoryError",
    })
}
