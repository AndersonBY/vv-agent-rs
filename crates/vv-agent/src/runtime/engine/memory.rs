use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::config::load_memory_summary_defaults_from_file;
use crate::llm::{LlmClient, LlmRequest};
use crate::memory::token_utils::resolve_model_token_limits_from_file;
use crate::memory::{
    MemoryManager, MemoryManagerConfig, SessionMemory, SessionMemoryConfig,
    SessionMemoryExtractionCallback, SummaryCallback,
};
use crate::types::{AgentTask, Message};

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
    let workspace = task.use_workspace.then_some(workspace_path.clone());
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

fn resolve_runtime_model_token_limits(
    settings_file: Option<&Path>,
    default_backend: Option<&str>,
    model: &str,
) -> (Option<u64>, Option<u64>) {
    let (Some(settings_file), Some(default_backend)) = (settings_file, default_backend) else {
        return (None, None);
    };
    resolve_model_token_limits_from_file(settings_file, default_backend, model)
}

fn build_session_memory<C>(
    task: &AgentTask,
    workspace: Option<PathBuf>,
    memory_summary_client: Option<C>,
    summary_backend: Option<String>,
    summary_model: String,
) -> Option<SessionMemory>
where
    C: LlmClient + Clone + 'static,
{
    if !session_memory_enabled(&task.metadata) && !task.metadata.contains_key("session_memory_seed")
    {
        return None;
    }
    let extraction_model = task
        .metadata
        .get("session_memory_extraction_model")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or(summary_model);
    let extraction_callback =
        memory_summary_client.map(|client| build_session_memory_extraction_callback(client));
    let storage_scope = read_optional_string_metadata(&task.metadata, &["session_id", "task_id"])
        .unwrap_or_else(|| task.task_id.clone());
    let mut session_memory = SessionMemory::with_workspace(
        SessionMemoryConfig {
            min_tokens_before_extraction: read_u64_metadata(
                &task.metadata,
                "session_memory_min_tokens",
                10_000,
            ),
            max_tokens: read_u64_metadata(&task.metadata, "session_memory_max_tokens", 40_000),
            min_text_messages: read_usize_metadata(
                &task.metadata,
                "session_memory_min_text_messages",
                5,
            ),
            growth_ratio: task
                .metadata
                .get("session_memory_growth_ratio")
                .and_then(parse_f64_metadata_value)
                .unwrap_or(0.5)
                .max(0.0),
            storage_dir: metadata_path(
                &task.metadata,
                "session_memory_storage_dir",
                ".memory/session",
            ),
            extraction_callback,
            extraction_backend: task
                .metadata
                .get("session_memory_extraction_backend")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or(summary_backend),
            extraction_model: Some(extraction_model),
            token_model: task.model.clone(),
        },
        workspace,
        Some(storage_scope),
    );
    session_memory.load();
    seed_session_memory(
        &mut session_memory,
        task.metadata.get("session_memory_seed"),
    );
    Some(session_memory)
}

fn build_memory_summary_callback<C>(client: C, default_model: String) -> SummaryCallback
where
    C: LlmClient + Clone + 'static,
{
    Arc::new(move |prompt, _backend, model| {
        let request_model = model.unwrap_or(&default_model).to_string();
        let response = client
            .clone()
            .complete(LlmRequest::new(request_model, vec![Message::user(prompt)]))
            .ok()?;
        let content = response.content.trim().to_string();
        (!content.is_empty()).then_some(content)
    })
}

fn build_session_memory_extraction_callback<C>(client: C) -> SessionMemoryExtractionCallback
where
    C: LlmClient + Clone + 'static,
{
    Arc::new(move |prompt, _backend, model| {
        let request = LlmRequest::new(
            model.unwrap_or_default(),
            vec![Message::user(prompt.to_string())],
        );
        client
            .complete(request)
            .ok()
            .map(|response| response.content.trim().to_string())
            .filter(|content| !content.is_empty())
    })
}

fn read_u64_metadata(metadata: &BTreeMap<String, Value>, key: &str, default: u64) -> u64 {
    metadata
        .get(key)
        .and_then(|value| match value {
            Value::Number(number) => number.as_u64(),
            Value::String(text) => text.trim().parse::<u64>().ok(),
            _ => None,
        })
        .unwrap_or(default)
}

fn read_usize_metadata(metadata: &BTreeMap<String, Value>, key: &str, default: usize) -> usize {
    read_u64_metadata(metadata, key, default as u64) as usize
}

fn read_f64_metadata(
    metadata: &BTreeMap<String, Value>,
    key: &str,
    default: f64,
    minimum: f64,
    maximum: Option<f64>,
) -> f64 {
    let mut value = metadata
        .get(key)
        .and_then(parse_f64_metadata_value)
        .unwrap_or(default)
        .max(minimum);
    if let Some(maximum) = maximum {
        value = value.min(maximum);
    }
    value
}

fn parse_f64_metadata_value(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn read_bool_metadata(metadata: &BTreeMap<String, Value>, key: &str, default: bool) -> bool {
    read_optional_bool_metadata(metadata, key).unwrap_or(default)
}

fn read_optional_bool_metadata(metadata: &BTreeMap<String, Value>, key: &str) -> Option<bool> {
    metadata.get(key).and_then(|value| match value {
        Value::Bool(flag) => Some(*flag),
        Value::Number(number) => match number.as_i64() {
            Some(0) => Some(false),
            Some(1) => Some(true),
            _ => None,
        },
        Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "y" | "on" => Some(true),
            "false" | "0" | "no" | "n" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    })
}

fn session_memory_enabled(metadata: &BTreeMap<String, Value>) -> bool {
    read_optional_bool_metadata(metadata, "session_memory_enabled")
        .or_else(|| read_optional_bool_metadata(metadata, "enable_session_memory"))
        .unwrap_or_else(|| !read_bool_metadata(metadata, "is_sub_task", false))
}

fn read_string_metadata(metadata: &BTreeMap<String, Value>, key: &str, default: &str) -> String {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default)
        .to_string()
}

fn read_optional_string_metadata(
    metadata: &BTreeMap<String, Value>,
    keys: &[&str],
) -> Option<String> {
    keys.iter().find_map(|key| {
        metadata
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn read_string_set_metadata(
    metadata: &BTreeMap<String, Value>,
    key: &str,
) -> Option<BTreeSet<String>> {
    let values = metadata.get(key)?.as_array()?;
    let values = values
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    (!values.is_empty()).then_some(values)
}

fn metadata_path(metadata: &BTreeMap<String, Value>, key: &str, default: &str) -> PathBuf {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

fn seed_session_memory(session_memory: &mut SessionMemory, value: Option<&Value>) {
    let Some(entries) = value.and_then(Value::as_array) else {
        return;
    };
    let parsed = entries
        .iter()
        .filter_map(|entry| {
            let object = entry.as_object()?;
            let content = object.get("content")?.as_str()?.trim();
            if content.is_empty() {
                return None;
            }
            let category = object
                .get("category")
                .and_then(Value::as_str)
                .unwrap_or("key_fact");
            let source_cycle = object
                .get("source_cycle")
                .and_then(Value::as_i64)
                .unwrap_or(0) as i32;
            let importance = object
                .get("importance")
                .and_then(Value::as_u64)
                .unwrap_or(5)
                .clamp(1, 10) as u8;
            Some(crate::memory::SessionMemoryEntry::new(
                category,
                content,
                source_cycle,
                importance,
            ))
        })
        .collect::<Vec<_>>();
    session_memory.merge_entries(parsed);
}
