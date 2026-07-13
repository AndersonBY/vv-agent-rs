use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::config::load_memory_summary_defaults_from_file;
use crate::events::RunEvent;
use crate::memory::token_utils::count_messages_tokens;
use crate::memory::{
    provider::block_on_memory_future, MemoryManager, MemoryManagerConfig, MemoryProvider,
};
use crate::model::ModelProvider;
use crate::runtime::context::ExecutionContext;
use crate::types::{AgentTask, Message};

mod callbacks;
mod metadata;
mod session;
mod token_limits;

use callbacks::build_memory_summary_callback;
use metadata::{
    metadata_path, read_bool_metadata, read_f64_metadata, read_optional_string_metadata,
    read_string_metadata, read_string_set_metadata, read_u64_metadata, read_usize_metadata,
};
use session::build_session_memory;
use token_limits::resolve_runtime_model_token_limits;

pub(super) fn build_memory_manager(
    task: &AgentTask,
    workspace_path: PathBuf,
    memory_model_provider: Option<Arc<dyn ModelProvider>>,
    settings_file: Option<&Path>,
    default_backend: Option<&str>,
) -> MemoryManager {
    let workspace = task.use_workspace.then_some(workspace_path);
    let local_summary_defaults = settings_file
        .map(load_memory_summary_defaults_from_file)
        .unwrap_or_default();
    let summary_backend = read_optional_string_metadata(
        &task.metadata,
        &[
            "memory_summary_backend",
            "compress_memory_summary_backend",
            "memory_compress_backend",
        ],
    )
    .or(local_summary_defaults.backend)
    .or_else(|| default_backend.map(str::to_string));
    let summary_model = read_optional_string_metadata(
        &task.metadata,
        &[
            "memory_summary_model",
            "compress_memory_summary_model",
            "memory_compress_model",
        ],
    )
    .or(local_summary_defaults.model)
    .unwrap_or_else(|| task.model.clone());
    let has_memory_route = summary_backend
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || read_optional_string_metadata(&task.metadata, &["session_memory_extraction_backend"])
            .is_some();
    let summary_callback = if has_memory_route {
        memory_model_provider.map(|provider| {
            build_memory_summary_callback(provider, summary_backend.clone(), summary_model.clone())
        })
    } else {
        None
    };
    let (resolved_context_window, resolved_max_output_tokens) =
        resolve_runtime_model_token_limits(settings_file, default_backend, &task.model);

    MemoryManager::new(MemoryManagerConfig {
        compact_threshold: task.memory_compact_threshold,
        keep_recent_messages: read_usize_metadata(
            &task.metadata,
            "memory_keep_recent_messages",
            10,
        ),
        model: task.model.clone(),
        model_context_window: read_u64_metadata(
            &task.metadata,
            "model_context_window",
            resolved_context_window.unwrap_or(200_000),
        ),
        reserved_output_tokens: read_u64_metadata(
            &task.metadata,
            "reserved_output_tokens",
            resolved_max_output_tokens.unwrap_or(16_000),
        ),
        autocompact_buffer_tokens: read_u64_metadata(
            &task.metadata,
            "autocompact_buffer_tokens",
            13_000,
        ),
        language: read_string_metadata(&task.metadata, "language", "zh-CN"),
        warning_threshold_percentage: task.memory_threshold_percentage.clamp(1, 100),
        include_memory_warning: read_bool_metadata(&task.metadata, "include_memory_warning", false),
        summary_event_limit: read_usize_metadata(&task.metadata, "summary_event_limit", 40),
        summary_backend: summary_backend.clone(),
        summary_model: Some(summary_model.clone()),
        summary_callback: summary_callback.clone(),
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
        tool_result_artifact_dir: metadata_path(
            &task.metadata,
            "tool_result_artifact_dir",
            ".memory/tool_results",
        ),
        microcompact_trigger_ratio: read_f64_metadata(
            &task.metadata,
            "microcompact_trigger_ratio",
            0.75,
            0.0,
            Some(1.0),
        ),
        microcompact_keep_recent_cycles: read_usize_metadata(
            &task.metadata,
            "microcompact_keep_recent_cycles",
            3,
        ),
        microcompact_min_result_length: read_usize_metadata(
            &task.metadata,
            "microcompact_min_result_length",
            500,
        ),
        microcompact_compactable_tools: read_string_set_metadata(
            &task.metadata,
            "microcompact_compactable_tools",
        ),
        workspace: workspace.clone(),
        session_memory: build_session_memory(
            task,
            workspace,
            summary_callback.clone(),
            summary_backend,
            summary_model,
        ),
    })
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
) -> Option<RunEvent> {
    if !force
        && !memory_manager.should_attempt_compaction(
            messages,
            previous_prompt_tokens,
            recent_tool_call_ids,
        )
    {
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
    cycle_index: u32,
    before_messages: &[Message],
    after_messages: &[Message],
    model: &str,
) -> RunEvent {
    let event = RunEvent::memory_compact_completed(
        started_event.run_id(),
        started_event.trace_id(),
        started_event
            .agent_name()
            .expect("memory compact event has agent identity"),
        cycle_index,
        before_messages.len(),
        after_messages.len(),
        Some(count_messages_tokens(after_messages, model)),
    );
    match started_event.session_id() {
        Some(session_id) => event.with_session_id(session_id),
        None => event,
    }
}

pub(super) fn memory_compact_event_payload(event: &RunEvent) -> BTreeMap<String, Value> {
    let mut payload = event.metadata().clone();
    if let Some(cycle_index) = event.cycle_index() {
        payload.insert("cycle".to_string(), Value::from(cycle_index));
    }
    match event.payload() {
        crate::events::RunEventPayload::MemoryCompactStarted {
            message_count,
            estimated_tokens,
        } => {
            payload.insert("message_count".to_string(), Value::from(*message_count));
            if let Some(estimated_tokens) = estimated_tokens {
                payload.insert(
                    "estimated_tokens".to_string(),
                    Value::from(*estimated_tokens),
                );
            }
        }
        crate::events::RunEventPayload::MemoryCompactCompleted {
            before_count,
            after_count,
            summary_tokens,
        } => {
            payload.insert("before_count".to_string(), Value::from(*before_count));
            payload.insert("after_count".to_string(), Value::from(*after_count));
            if let Some(summary_tokens) = summary_tokens {
                payload.insert("summary_tokens".to_string(), Value::from(*summary_tokens));
            }
        }
        _ => {}
    }
    payload
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
