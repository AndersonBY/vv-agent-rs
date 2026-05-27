use std::collections::BTreeSet;
use std::path::PathBuf;

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

#[derive(Debug, Clone)]
pub struct MemoryManagerConfig {
    pub compact_threshold: u64,
    pub keep_recent_messages: usize,
    pub model: String,
    pub model_context_window: u64,
    pub reserved_output_tokens: u64,
    pub autocompact_buffer_tokens: u64,
    pub summary_event_limit: usize,
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

impl Default for MemoryManagerConfig {
    fn default() -> Self {
        Self {
            compact_threshold: 128_000,
            keep_recent_messages: 10,
            model: String::new(),
            model_context_window: 200_000,
            reserved_output_tokens: 16_000,
            autocompact_buffer_tokens: 13_000,
            summary_event_limit: 40,
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

    pub fn compact(&mut self, messages: &[Message], force: bool) -> (Vec<Message>, bool) {
        self.compact_for_cycle(messages, 0, force)
    }

    pub fn compact_for_cycle(
        &mut self,
        messages: &[Message],
        cycle_index: u32,
        force: bool,
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
        let message_length = count_messages_tokens(&sanitized, &self.config.model);
        if !force && message_length <= self.autocompact_threshold() {
            if self.should_preemptive_microcompact(message_length) {
                let (microcompacted, cleared) = self.microcompact_messages(&sanitized, cycle_index);
                if cleared > 0 {
                    return (microcompacted, true);
                }
            }
            return (sanitized, changed_by_sanitize);
        }
        if let Some(session_memory) = self.session_memory.as_mut() {
            let text_messages = sanitized
                .iter()
                .filter(|message| {
                    !matches!(message.role, MessageRole::System | MessageRole::Tool)
                        && !message.content.trim().is_empty()
                })
                .count();
            if session_memory.should_extract(message_length, text_messages) {
                session_memory.extract(&sanitized, 0, message_length);
            }
        }
        let (compacted, changed) = self.compress_memory(&sanitized);
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

    fn microcompact_trigger_threshold(&self) -> u64 {
        let ratio = self.config.microcompact_trigger_ratio.clamp(0.0, 1.0);
        (self.autocompact_threshold() as f64 * ratio).floor() as u64
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
        if messages.len() <= 2 {
            return (messages.to_vec(), false);
        }
        let system_message = messages
            .iter()
            .find(|message| message.role == MessageRole::System)
            .cloned();
        let (messages_for_summary, _normalized) = self.normalize_compaction_messages(messages);
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
        let original_request = extract_original_user_request(messages).unwrap_or_default();
        let summary =
            LocalSummary::from_messages(&messages_for_summary, self.config.summary_event_limit);
        let mut compressed_memory = summary.to_json_string();
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
