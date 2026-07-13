use std::path::PathBuf;

use serde_json::Value;

use crate::memory::{SessionMemory, SessionMemoryConfig, SessionMemoryExtractionCallback};
use crate::types::AgentTask;

use super::metadata::{
    metadata_path, parse_f64_metadata_value, read_optional_string_metadata, read_u64_metadata,
    read_usize_metadata, session_memory_enabled,
};

pub(super) fn build_session_memory(
    task: &AgentTask,
    workspace: Option<PathBuf>,
    extraction_callback: Option<SessionMemoryExtractionCallback>,
    summary_backend: Option<String>,
    summary_model: String,
) -> Option<SessionMemory> {
    if !session_memory_enabled(&task.metadata) && !task.metadata.contains_key("session_memory_seed")
    {
        return None;
    }
    let extraction_model =
        read_optional_string_metadata(&task.metadata, &["session_memory_extraction_model"])
            .unwrap_or(summary_model);
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
            extraction_backend: read_optional_string_metadata(
                &task.metadata,
                &["session_memory_extraction_backend"],
            )
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
