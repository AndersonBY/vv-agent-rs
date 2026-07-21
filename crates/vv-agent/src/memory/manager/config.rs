use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use crate::memory::session::SessionMemory;

pub type SummaryCallback =
    Arc<dyn Fn(&str, Option<&str>, Option<&str>) -> Option<String> + Send + Sync + 'static>;

#[derive(Clone)]
pub struct MemoryManagerConfig {
    pub compact_threshold: u64,
    pub keep_recent_messages: usize,
    pub model: String,
    pub model_context_window: u64,
    pub reserved_output_tokens: u64,
    pub autocompact_buffer_tokens: u64,
    pub language: String,
    pub warning_threshold_percentage: u8,
    pub include_memory_warning: bool,
    pub summary_event_limit: usize,
    pub summary_backend: Option<String>,
    pub summary_model: Option<String>,
    pub summary_callback: Option<SummaryCallback>,
    pub tool_result_compact_threshold: usize,
    pub tool_result_keep_last: usize,
    pub tool_result_excerpt_head: usize,
    pub tool_result_excerpt_tail: usize,
    pub tool_calls_keep_last: usize,
    pub assistant_no_tool_keep_last: usize,
    pub tool_result_artifact_dir: PathBuf,
    pub microcompact_trigger_ratio: f64,
    pub microcompact_keep_recent_cycles: usize,
    pub microcompact_min_result_length: usize,
    pub microcompact_compactable_tools: Option<BTreeSet<String>>,
    pub workspace: Option<PathBuf>,
    pub session_memory: Option<SessionMemory>,
}

impl fmt::Debug for MemoryManagerConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemoryManagerConfig")
            .field("compact_threshold", &self.compact_threshold)
            .field("keep_recent_messages", &self.keep_recent_messages)
            .field("model", &self.model)
            .field("model_context_window", &self.model_context_window)
            .field("reserved_output_tokens", &self.reserved_output_tokens)
            .field("autocompact_buffer_tokens", &self.autocompact_buffer_tokens)
            .field("language", &self.language)
            .field(
                "warning_threshold_percentage",
                &self.warning_threshold_percentage,
            )
            .field("include_memory_warning", &self.include_memory_warning)
            .field("summary_event_limit", &self.summary_event_limit)
            .field("summary_backend", &self.summary_backend)
            .field("summary_model", &self.summary_model)
            .field(
                "summary_callback",
                &self.summary_callback.as_ref().map(|_| "<callback>"),
            )
            .field(
                "tool_result_compact_threshold",
                &self.tool_result_compact_threshold,
            )
            .field("tool_result_keep_last", &self.tool_result_keep_last)
            .field("tool_result_excerpt_head", &self.tool_result_excerpt_head)
            .field("tool_result_excerpt_tail", &self.tool_result_excerpt_tail)
            .field("tool_calls_keep_last", &self.tool_calls_keep_last)
            .field(
                "assistant_no_tool_keep_last",
                &self.assistant_no_tool_keep_last,
            )
            .field("tool_result_artifact_dir", &self.tool_result_artifact_dir)
            .field(
                "microcompact_trigger_ratio",
                &self.microcompact_trigger_ratio,
            )
            .field(
                "microcompact_keep_recent_cycles",
                &self.microcompact_keep_recent_cycles,
            )
            .field(
                "microcompact_min_result_length",
                &self.microcompact_min_result_length,
            )
            .field(
                "microcompact_compactable_tools",
                &self.microcompact_compactable_tools,
            )
            .field("workspace", &self.workspace)
            .field("session_memory", &self.session_memory)
            .finish()
    }
}

impl Default for MemoryManagerConfig {
    fn default() -> Self {
        Self {
            compact_threshold: 250_000,
            keep_recent_messages: 10,
            model: String::new(),
            model_context_window: 200_000,
            reserved_output_tokens: 16_000,
            autocompact_buffer_tokens: 13_000,
            language: "zh-CN".to_string(),
            warning_threshold_percentage: 90,
            include_memory_warning: false,
            summary_event_limit: 40,
            summary_backend: None,
            summary_model: None,
            summary_callback: None,
            tool_result_compact_threshold: 2_000,
            tool_result_keep_last: 3,
            tool_result_excerpt_head: 200,
            tool_result_excerpt_tail: 200,
            tool_calls_keep_last: 3,
            assistant_no_tool_keep_last: 1,
            tool_result_artifact_dir: PathBuf::from(".memory/tool_results"),
            microcompact_trigger_ratio: 0.75,
            microcompact_keep_recent_cycles: 3,
            microcompact_min_result_length: 500,
            microcompact_compactable_tools: None,
            workspace: None,
            session_memory: None,
        }
    }
}
