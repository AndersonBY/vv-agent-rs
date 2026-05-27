use std::collections::BTreeSet;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::Arc;

use crate::memory::artifacts::{
    compact_tool_results, render_persisted_artifacts_section, ToolResultArtifactConfig,
};
use crate::memory::message_sanitizer::filter_empty_assistant_messages;
use crate::memory::microcompact::{microcompact, MicrocompactConfig};
use crate::memory::post_compact_restore::{restore_key_files, PostCompactRestoreConfig};
use crate::memory::session::SessionMemory;
use crate::memory::summary::LocalSummary;
use crate::memory::token_utils::{compute_compaction_threshold, count_messages_tokens};
use crate::types::{Message, MessageRole};

const MEMORY_SUMMARY_NAME: &str = "memory_summary";
const MEMORY_WARNING_EN: &str = "The current memory usage has exceeded {memory_threshold_percentage}%. It is recommended to immediately organize and record key information and materials from the conversation, and store them in the workspace to prevent data loss after memory compression.\n\n";
const MEMORY_WARNING_ZH: &str = "当前记忆已使用容量超过 {memory_threshold_percentage}%,建议立即整理、记录对话中的关键信息、资料, 并储存至工作区, 避免记忆压缩后资料丢失。\n\n";
const COMPRESS_MEMORY_PROMPT_EN: &str = r#"You are summarizing a conversation between a user and an AI coding assistant.
Provide your analysis in <analysis> tags first (this section will be stripped), then output a structured JSON summary.

<analysis>
Think step by step about what information is critical to preserve, especially the user's exact wording,
the current work state, file operations, and any errors that were resolved.
</analysis>

<Conversation History>
{messages}
</Conversation History>

Please compress the conversation into a structured JSON "Task Status Summary".
This summary should allow the Agent to quickly resume the task
while preserving user constraints, key decisions, file operations, and critical context.

Requirements:
- Output JSON only, no Markdown.
- Keep fields concise and searchable; use short sentences.
- If a field has no data, use [] or "" as appropriate.
- The "original_user_messages" field is critical. Preserve user messages verbatim or near-verbatim.

JSON Schema:
{
  "summary_version": "2.0",
  "original_user_messages": ["..."],
  "user_constraints": ["..."],
  "decisions": ["..."],
  "files_examined_or_modified": [
    {"path": "...", "action": "read|created|modified|deleted", "summary": "..."}
  ],
  "errors_and_fixes": [
    {"error": "...", "fix": "...", "file": "..."}
  ],
  "progress": ["Preserve up to {event_limit} critical events"],
  "key_facts": ["..."],
  "open_issues": ["..."],
  "current_work_state": "...",
  "next_steps": ["..."]
}
"#;
const COMPRESS_MEMORY_PROMPT_ZH: &str = r#"你正在总结一段用户与 AI 编程助手的对话。
请先在 <analysis> 标签中进行思考 (该部分后续会被剥离), 然后输出结构化 JSON 摘要。

<analysis>
请逐步思考: 哪些信息必须保留, 哪些用户原话不能丢, 哪些文件/错误/当前状态会影响后续继续执行。
</analysis>

<Conversation History>
{messages}
</Conversation History>

请将以上对话压缩为结构化 JSON「Task Status Summary」, 让 Agent 能快速恢复任务, 并保留用户约束、关键决策、文件操作与当前工作状态。

要求:
- 只输出 JSON, 不要 Markdown。
- 字段内容简洁、可检索, 短句表达。
- 没有信息的字段使用 [] 或 ""。
- `original_user_messages` 字段至关重要: 尽量保留用户原话, 不要做概括式改写。

JSON Schema:
{
  "summary_version": "2.0",
  "original_user_messages": ["..."],
  "user_constraints": ["..."],
  "decisions": ["..."],
  "files_examined_or_modified": [
    {"path": "...", "action": "read|created|modified|deleted", "summary": "..."}
  ],
  "errors_and_fixes": [
    {"error": "...", "fix": "...", "file": "..."}
  ],
  "progress": ["最多保留 {event_limit} 条关键进展"],
  "key_facts": ["..."],
  "open_issues": ["..."],
  "current_work_state": "...",
  "next_steps": ["..."]
}
"#;

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
            compact_threshold: 128_000,
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

#[derive(Debug, Clone)]
pub struct MemoryManager {
    pub config: MemoryManagerConfig,
    session_memory: Option<SessionMemory>,
}

impl MemoryManager {
    pub fn new(mut config: MemoryManagerConfig) -> Self {
        let session_memory = config.session_memory.take();
        Self {
            config,
            session_memory,
        }
    }

    pub fn autocompact_threshold(&self) -> u64 {
        compute_compaction_threshold(
            self.config.compact_threshold,
            self.config.model_context_window,
            self.config.reserved_output_tokens,
            self.config.autocompact_buffer_tokens,
        )
    }

    pub fn effective_context_window(&self) -> u64 {
        self.config
            .model_context_window
            .saturating_sub(self.config.reserved_output_tokens)
    }

    pub fn compact(&mut self, messages: &[Message], force: bool) -> (Vec<Message>, bool) {
        self.compact_for_cycle_with_usage(messages, 0, force, None, None)
    }

    pub fn compact_for_cycle(
        &mut self,
        messages: &[Message],
        cycle_index: u32,
        force: bool,
    ) -> (Vec<Message>, bool) {
        self.compact_for_cycle_with_usage(messages, cycle_index, force, None, None)
    }

    pub fn compact_for_cycle_with_usage(
        &mut self,
        messages: &[Message],
        cycle_index: u32,
        force: bool,
        total_tokens: Option<u64>,
        recent_tool_call_ids: Option<&BTreeSet<String>>,
    ) -> (Vec<Message>, bool) {
        if messages.is_empty() {
            return (Vec::new(), false);
        }

        let cleaned = self.remove_previous_summary(messages);
        let sanitized = filter_empty_assistant_messages(&cleaned);
        let changed_by_sanitize = sanitized.len() != messages.len()
            || sanitized
                .iter()
                .zip(messages.iter())
                .any(|(left, right)| left != right);
        let working_messages = self.apply_session_memory_context(&sanitized);
        let message_length =
            self.calculate_effective_length(&working_messages, total_tokens, recent_tool_call_ids);
        if !force && message_length <= self.autocompact_threshold() {
            let (warned, warning_inserted) =
                self.maybe_append_memory_warning(&working_messages, message_length);
            if warning_inserted {
                return (warned, true);
            }
            if self.should_preemptive_microcompact(message_length) {
                let (microcompacted, cleared) =
                    self.microcompact_messages(&working_messages, cycle_index);
                if cleared > 0 {
                    return (microcompacted, true);
                }
            }
            return (working_messages, changed_by_sanitize);
        }
        if let Some(session_memory) = self.session_memory.as_mut() {
            let text_messages = working_messages
                .iter()
                .filter(|message| {
                    !matches!(message.role, MessageRole::System | MessageRole::Tool)
                        && !message.content.trim().is_empty()
                })
                .count();
            if session_memory.should_extract(message_length, text_messages) {
                session_memory.extract(&working_messages, 0, message_length);
            }
        }
        let mut summary_source = self.strip_session_memory_context(&working_messages);
        if !force {
            let (image_compacted, image_changed) =
                compact_processed_image_messages(&working_messages);
            let (artifact_compacted, artifact_changed) =
                self.compact_large_tool_results(&image_compacted);
            if (image_changed || artifact_changed)
                && count_messages_tokens(&artifact_compacted, &self.config.model)
                    <= self.autocompact_threshold()
            {
                return (artifact_compacted, true);
            }
            if image_changed {
                summary_source = image_compacted;
            }
        }
        let (compacted, changed) = self.compress_memory(&summary_source);
        if changed {
            if let Some(session_memory) = self.session_memory.as_mut() {
                session_memory
                    .on_compaction(Some(count_messages_tokens(&compacted, &self.config.model)));
            }
        }
        (compacted, changed)
    }

    pub fn should_preemptive_microcompact(&self, message_length: u64) -> bool {
        let threshold = self.microcompact_trigger_threshold();
        threshold > 0 && message_length > threshold
    }

    pub fn estimate_memory_usage_percentage(
        &self,
        messages: &[Message],
        total_tokens: Option<u64>,
        recent_tool_call_ids: Option<&BTreeSet<String>>,
    ) -> u64 {
        let threshold = self.autocompact_threshold();
        if threshold == 0 {
            return 0;
        }
        let used_tokens =
            self.calculate_effective_length(messages, total_tokens, recent_tool_call_ids);
        (used_tokens.saturating_mul(100)) / threshold
    }

    pub fn warning_threshold(&self) -> u64 {
        let threshold = self.autocompact_threshold();
        if threshold == 0 {
            return 0;
        }
        (threshold * u64::from(self.config.warning_threshold_percentage)) / 100
    }

    pub fn microcompact_messages(
        &self,
        messages: &[Message],
        cycle_index: u32,
    ) -> (Vec<Message>, usize) {
        microcompact(
            messages,
            cycle_index,
            &MicrocompactConfig {
                trigger_ratio: self.config.microcompact_trigger_ratio,
                keep_recent_cycles: self.config.microcompact_keep_recent_cycles,
                min_result_length: self.config.microcompact_min_result_length,
                compactable_tools: self.config.microcompact_compactable_tools.clone(),
            },
        )
    }

    pub fn microcompact_trigger_threshold(&self) -> u64 {
        let ratio = self.config.microcompact_trigger_ratio.clamp(0.0, 1.0);
        (self.autocompact_threshold() as f64 * ratio).floor() as u64
    }

    fn calculate_effective_length(
        &self,
        messages: &[Message],
        total_tokens: Option<u64>,
        recent_tool_call_ids: Option<&BTreeSet<String>>,
    ) -> u64 {
        if let Some(total_tokens) = total_tokens.filter(|tokens| *tokens > 0) {
            return total_tokens
                + self.estimate_recent_tool_message_length(messages, recent_tool_call_ids);
        }
        count_messages_tokens(messages, &self.config.model)
    }

    fn estimate_recent_tool_message_length(
        &self,
        messages: &[Message],
        recent_tool_call_ids: Option<&BTreeSet<String>>,
    ) -> u64 {
        let Some(recent_tool_call_ids) = recent_tool_call_ids.filter(|ids| !ids.is_empty()) else {
            return 0;
        };
        let tool_messages = messages
            .iter()
            .filter(|message| {
                message.role == MessageRole::Tool
                    && message
                        .tool_call_id
                        .as_ref()
                        .is_some_and(|tool_call_id| recent_tool_call_ids.contains(tool_call_id))
            })
            .cloned()
            .collect::<Vec<_>>();
        count_messages_tokens(&tool_messages, &self.config.model)
    }

    fn maybe_append_memory_warning(
        &self,
        messages: &[Message],
        message_length: u64,
    ) -> (Vec<Message>, bool) {
        if !self.config.include_memory_warning || self.autocompact_threshold() == 0 {
            return (messages.to_vec(), false);
        }
        if message_length < self.warning_threshold() {
            return (messages.to_vec(), false);
        }
        let warning_text = self.memory_warning_text();
        if messages.iter().rev().take(10).any(|message| {
            message.role == MessageRole::User && message.content.contains(&warning_text)
        }) {
            return (messages.to_vec(), false);
        }
        let mut warned = messages.to_vec();
        warned.push(Message::user(warning_text));
        (warned, true)
    }

    fn memory_warning_text(&self) -> String {
        let template = if self.config.language == "zh-CN" {
            MEMORY_WARNING_ZH
        } else {
            MEMORY_WARNING_EN
        };
        template.replace(
            "{memory_threshold_percentage}",
            &self.config.warning_threshold_percentage.to_string(),
        )
    }

    fn compact_large_tool_results(&self, messages: &[Message]) -> (Vec<Message>, bool) {
        let (compacted, _artifacts, changed) = compact_tool_results(
            messages,
            &ToolResultArtifactConfig {
                workspace: self.config.workspace.clone(),
                artifact_dir: self.config.tool_result_artifact_dir.clone(),
                compact_threshold: self.config.tool_result_compact_threshold,
                keep_last: self.config.tool_result_keep_last,
                excerpt_head: self.config.tool_result_excerpt_head,
                excerpt_tail: self.config.tool_result_excerpt_tail,
            },
        );
        (compacted, changed)
    }

    pub fn emergency_compact(&self, messages: &[Message], drop_ratio: f64) -> Vec<Message> {
        if messages.len() <= 2 {
            return messages.to_vec();
        }

        let (system_message, non_system) = if messages
            .first()
            .is_some_and(|message| message.role == MessageRole::System)
        {
            (messages.first().cloned(), &messages[1..])
        } else {
            (None, messages)
        };
        if non_system.is_empty() {
            return system_message.into_iter().collect();
        }

        let keep_count = self.config.keep_recent_messages.max(1);
        let clamped_ratio = drop_ratio.clamp(0.0, 0.95);
        let max_droppable = non_system.len().saturating_sub(keep_count);
        let drop_count = if max_droppable == 0 {
            0
        } else {
            ((non_system.len() as f64 * clamped_ratio).floor() as usize)
                .max(1)
                .min(max_droppable)
        };
        let mut start_index = drop_count.min(non_system.len());
        if non_system.len().saturating_sub(start_index) < keep_count {
            start_index = non_system.len().saturating_sub(keep_count);
        }
        start_index = adjust_start_for_tool_context(non_system, start_index);

        let mut compacted = Vec::new();
        if let Some(system_message) = system_message {
            compacted.push(system_message);
        }
        compacted.extend_from_slice(&non_system[start_index..]);
        sanitize_empty_assistant_messages(compacted)
    }

    pub fn session_memory(&self) -> Option<&SessionMemory> {
        self.session_memory.as_ref()
    }

    pub fn session_memory_mut(&mut self) -> Option<&mut SessionMemory> {
        self.session_memory.as_mut()
    }

    pub fn apply_session_memory_context(&self, messages: &[Message]) -> Vec<Message> {
        let Some(session_context) = self
            .session_memory
            .as_ref()
            .map(SessionMemory::render_as_system_context)
            .filter(|context| !context.is_empty())
        else {
            return messages.to_vec();
        };
        let mut updated = messages.to_vec();
        if let Some(system_message) = updated
            .iter_mut()
            .find(|message| message.role == MessageRole::System)
        {
            if !system_message.content.contains("<Session Memory>") {
                system_message.content.push_str("\n\n");
                system_message.content.push_str(&session_context);
            }
            return updated;
        }
        let mut system_message = Message::system(session_context);
        system_message.name = Some("session_memory".to_string());
        updated.insert(0, system_message);
        updated
    }

    pub fn strip_session_memory_context(&self, messages: &[Message]) -> Vec<Message> {
        let mut updated = messages.to_vec();
        let Some(system_message) = updated
            .iter_mut()
            .find(|message| message.role == MessageRole::System)
        else {
            return updated;
        };
        let Some(marker_index) = system_message.content.find("<Session Memory>") else {
            return updated;
        };
        system_message.content = system_message.content[..marker_index]
            .trim_end()
            .to_string();
        updated
    }

    fn remove_previous_summary(&self, messages: &[Message]) -> Vec<Message> {
        messages
            .iter()
            .filter(|message| {
                !(message.role == MessageRole::System
                    && message.name.as_deref() == Some(MEMORY_SUMMARY_NAME))
            })
            .cloned()
            .collect()
    }

    fn compress_memory(&self, messages: &[Message]) -> (Vec<Message>, bool) {
        let messages = self.strip_session_memory_context(messages);
        if messages.len() <= 2 {
            return (messages, false);
        }
        let system_message = messages
            .iter()
            .find(|message| message.role == MessageRole::System)
            .cloned();
        let (messages_for_summary, _normalized) = self.normalize_compaction_messages(&messages);
        let (messages_for_summary, artifacts, _compacted_tools) = compact_tool_results(
            &messages_for_summary,
            &ToolResultArtifactConfig {
                workspace: self.config.workspace.clone(),
                artifact_dir: self.config.tool_result_artifact_dir.clone(),
                compact_threshold: self.config.tool_result_compact_threshold,
                keep_last: self.config.tool_result_keep_last,
                excerpt_head: self.config.tool_result_excerpt_head,
                excerpt_tail: self.config.tool_result_excerpt_tail,
            },
        );
        let original_request = extract_original_user_request(&messages).unwrap_or_default();
        let summary_prompt = self.build_compress_memory_prompt(&messages_for_summary);
        let mut compressed_memory = self.generate_summary(&summary_prompt, &messages_for_summary);
        if let Ok(summary_data) = serde_json::from_str(&compressed_memory) {
            let restored_context = restore_key_files(
                &summary_data,
                self.config.workspace.as_deref(),
                &PostCompactRestoreConfig {
                    token_model: self.config.model.clone(),
                    ..PostCompactRestoreConfig::default()
                },
            );
            if !restored_context.is_empty() {
                compressed_memory.push_str("\n\n");
                compressed_memory.push_str(&restored_context);
            }
        }
        if let Some(artifact_section) = render_persisted_artifacts_section(&artifacts) {
            compressed_memory.push_str("\n\n");
            compressed_memory.push_str(&artifact_section);
        }

        let mut compacted = Vec::new();
        if let Some(system_message) = system_message {
            compacted.push(system_message);
        }
        compacted.push(Message::user(format!(
            "<Original User Request>\n{original_request}\n</Original User Request>\n\n<Compressed Agent Memory>\n{compressed_memory}\n</Compressed Agent Memory>"
        )));
        (compacted, true)
    }

    fn build_compress_memory_prompt(&self, messages: &[Message]) -> String {
        let template = if self.config.language == "zh-CN" {
            COMPRESS_MEMORY_PROMPT_ZH
        } else {
            COMPRESS_MEMORY_PROMPT_EN
        };
        let serialized_messages = messages
            .iter()
            .map(|message| message.to_openai_message(true))
            .collect::<Vec<_>>();
        template
            .replace(
                "{messages}",
                &serde_json::to_string(&serialized_messages).unwrap_or_default(),
            )
            .replace(
                "{event_limit}",
                &self.config.summary_event_limit.max(1).to_string(),
            )
    }

    fn generate_summary(&self, prompt: &str, messages: &[Message]) -> String {
        if let Some(callback) = &self.config.summary_callback {
            let callback_result = catch_unwind(AssertUnwindSafe(|| {
                callback(
                    prompt,
                    self.config.summary_backend.as_deref(),
                    self.config.summary_model.as_deref(),
                )
            }));
            if let Ok(Some(summary)) = callback_result {
                let normalized = normalize_summary_output(&summary);
                if !normalized.trim().is_empty() {
                    return normalized;
                }
            }
        }
        LocalSummary::from_messages(messages, self.config.summary_event_limit).to_json_string()
    }

    fn normalize_compaction_messages(&self, messages: &[Message]) -> (Vec<Message>, bool) {
        let (messages, stripped) = self.strip_stale_tool_calls(messages);
        let (messages, normalized) = normalize_orphan_tool_messages(&messages);
        let (messages, collapsed) = self.collapse_assistant_no_tool_messages(&messages);
        let sanitized = filter_empty_assistant_messages(&messages);
        let sanitized_changed = sanitized.len() != messages.len()
            || sanitized
                .iter()
                .zip(messages.iter())
                .any(|(left, right)| left != right);
        (
            sanitized,
            stripped || normalized || collapsed || sanitized_changed,
        )
    }

    fn strip_stale_tool_calls(&self, messages: &[Message]) -> (Vec<Message>, bool) {
        let keep_count = self.config.tool_calls_keep_last;
        let tool_call_indices = messages
            .iter()
            .enumerate()
            .filter_map(|(index, message)| {
                (message.role == MessageRole::Assistant && !message.tool_calls.is_empty())
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        let keep_indices = if keep_count == 0 {
            Vec::new()
        } else {
            tool_call_indices
                .iter()
                .rev()
                .take(keep_count)
                .copied()
                .collect::<Vec<_>>()
        };

        let mut changed = false;
        let mut stripped = Vec::with_capacity(messages.len());
        for (index, message) in messages.iter().enumerate() {
            if message.role == MessageRole::Assistant
                && !message.tool_calls.is_empty()
                && !keep_indices.contains(&index)
            {
                changed = true;
                let mut updated = message.clone();
                updated.tool_calls.clear();
                if updated.content.trim().is_empty() {
                    continue;
                }
                stripped.push(updated);
            } else {
                stripped.push(message.clone());
            }
        }
        (stripped, changed)
    }

    fn collapse_assistant_no_tool_messages(&self, messages: &[Message]) -> (Vec<Message>, bool) {
        let keep_last = self.config.assistant_no_tool_keep_last;
        if keep_last == 0 {
            return (messages.to_vec(), false);
        }
        let mut changed = false;
        let mut collapsed = Vec::with_capacity(messages.len());
        let mut run_buffer = Vec::<Message>::new();
        for message in messages {
            if message.role == MessageRole::Assistant && message.tool_calls.is_empty() {
                run_buffer.push(message.clone());
                continue;
            }
            flush_assistant_run(&mut collapsed, &mut run_buffer, keep_last, &mut changed);
            collapsed.push(message.clone());
        }
        flush_assistant_run(&mut collapsed, &mut run_buffer, keep_last, &mut changed);
        (collapsed, changed)
    }
}

fn sanitize_empty_assistant_messages(messages: Vec<Message>) -> Vec<Message> {
    filter_empty_assistant_messages(&messages)
}

fn normalize_summary_output(text: &str) -> String {
    let mut cleaned = strip_markdown_code_fence(text);
    let analysis_pattern =
        regex::Regex::new(r"(?is)<analysis>.*?</analysis>").expect("analysis regex");
    cleaned = analysis_pattern
        .replace_all(&cleaned, "")
        .trim()
        .to_string();
    let summary_pattern =
        regex::Regex::new(r"(?is)<summary>\s*(.*?)\s*</summary>").expect("summary regex");
    if let Some(captures) = summary_pattern.captures(&cleaned) {
        return captures
            .get(1)
            .map(|matched| matched.as_str().trim().to_string())
            .unwrap_or_default();
    }
    cleaned
}

fn strip_markdown_code_fence(text: &str) -> String {
    let cleaned = text.trim();
    if !cleaned.starts_with("```") {
        return cleaned.to_string();
    }
    let mut lines = cleaned.lines().collect::<Vec<_>>();
    if lines.len() < 2 {
        return cleaned.to_string();
    }
    lines.remove(0);
    if lines
        .last()
        .is_some_and(|line| line.trim().starts_with("```"))
    {
        lines.pop();
    }
    lines.join("\n").trim().to_string()
}

fn normalize_orphan_tool_messages(messages: &[Message]) -> (Vec<Message>, bool) {
    let mut changed = false;
    let mut pending_tool_calls = std::collections::BTreeMap::<String, usize>::new();
    let mut normalized = Vec::with_capacity(messages.len());
    for message in messages {
        if message.role == MessageRole::Assistant && !message.tool_calls.is_empty() {
            for tool_call in &message.tool_calls {
                let tool_call_id = tool_call.id.trim();
                if tool_call_id.is_empty() {
                    continue;
                }
                *pending_tool_calls
                    .entry(tool_call_id.to_string())
                    .or_default() += 1;
            }
            normalized.push(message.clone());
            continue;
        }

        if message.role == MessageRole::Tool {
            let tool_call_id = message.tool_call_id.as_deref().unwrap_or_default().trim();
            if tool_call_id.is_empty() {
                changed = true;
                continue;
            }
            let remaining = pending_tool_calls.get(tool_call_id).copied().unwrap_or(0);
            if remaining == 0 {
                changed = true;
                continue;
            }
            pending_tool_calls.insert(tool_call_id.to_string(), remaining - 1);
        }
        normalized.push(message.clone());
    }
    (normalized, changed)
}

fn flush_assistant_run(
    collapsed: &mut Vec<Message>,
    run_buffer: &mut Vec<Message>,
    keep_last: usize,
    changed: &mut bool,
) {
    if run_buffer.is_empty() {
        return;
    }
    if run_buffer.len() > keep_last {
        *changed = true;
        let start = run_buffer.len() - keep_last;
        collapsed.extend(run_buffer[start..].iter().cloned());
    } else {
        collapsed.append(run_buffer);
        return;
    }
    run_buffer.clear();
}

fn extract_original_user_request(messages: &[Message]) -> Option<String> {
    messages
        .iter()
        .skip(1)
        .find(|message| message.role == MessageRole::User && !message.content.trim().is_empty())
        .map(|message| {
            let content = message.content.trim();
            if let Some(extracted) = extract_between(
                content,
                "<Original User Request>",
                "</Original User Request>",
            ) {
                extracted.to_string()
            } else {
                content.to_string()
            }
        })
}

fn extract_between<'a>(text: &'a str, start_marker: &str, end_marker: &str) -> Option<&'a str> {
    let start = text.find(start_marker)?;
    let rest = &text[start + start_marker.len()..];
    let end = rest.find(end_marker)?;
    Some(rest[..end].trim())
}

fn adjust_start_for_tool_context(messages: &[Message], mut start_index: usize) -> usize {
    while start_index > 0 && start_index < messages.len() {
        let message = &messages[start_index];
        if message.role != MessageRole::Tool {
            break;
        }
        start_index -= 1;
    }
    start_index
}

fn compact_processed_image_messages(messages: &[Message]) -> (Vec<Message>, bool) {
    let assistant_indices = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| (message.role == MessageRole::Assistant).then_some(index))
        .collect::<Vec<_>>();
    if assistant_indices.is_empty() {
        return (messages.to_vec(), false);
    }

    let mut changed = false;
    let compacted = messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            if message.role == MessageRole::User
                && message.image_url.is_some()
                && assistant_indices
                    .iter()
                    .any(|assistant_index| *assistant_index > index)
            {
                changed = true;
                let mut updated = message.clone();
                updated.image_url = None;
                updated.content = format!("{} [image payload compacted]", updated.content)
                    .trim()
                    .to_string();
                updated
            } else {
                message.clone()
            }
        })
        .collect::<Vec<_>>();
    (compacted, changed)
}
