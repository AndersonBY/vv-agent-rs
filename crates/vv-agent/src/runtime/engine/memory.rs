use std::path::{Path, PathBuf};

use crate::config::load_memory_summary_defaults_from_file;
use crate::llm::LlmClient;
use crate::memory::{MemoryManager, MemoryManagerConfig};
use crate::types::AgentTask;

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

pub(super) fn build_memory_manager<C>(
    task: &AgentTask,
    workspace_path: PathBuf,
    memory_summary_client: Option<C>,
    settings_file: Option<&Path>,
    default_backend: Option<&str>,
) -> MemoryManager
where
    C: LlmClient + Clone + 'static,
{
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
    let summary_callback = if settings_file.is_some()
        && summary_backend
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    {
        memory_summary_client
            .clone()
            .map(|client| build_memory_summary_callback(client, summary_model.clone()))
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
        summary_callback,
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
            memory_summary_client,
            summary_backend,
            summary_model,
        ),
    })
}
