use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

mod compaction;
mod config;
mod emergency;
mod helpers;
mod limits;
mod microcompact;
mod normalization;
mod prompts;
mod session_context;
mod warnings;

use crate::events::{MemoryCompactMode, ReservedOutputSource};
use crate::memory::message_sanitizer::filter_empty_assistant_messages;
use crate::memory::session::SessionMemory;
use crate::memory::token_utils::count_messages_tokens;
use crate::memory::{RuntimeMemoryCallbackError, RuntimeMemoryCallbacks};
use crate::tools::ToolResultRetention;
use crate::types::{Message, MessageRole};
use crate::workspace::WorkspaceBackend;

pub use config::{MemoryManagerConfig, SummaryCallback};

use helpers::compact_processed_image_messages;

const MEMORY_SUMMARY_NAME: &str = "memory_summary";

#[derive(Clone)]
pub struct MemoryManager {
    pub config: MemoryManagerConfig,
    session_memory: Option<SessionMemory>,
    model_max_output_tokens: Option<u64>,
    reserved_output_source: ReservedOutputSource,
    runtime_callbacks: RuntimeMemoryCallbacks,
    workspace_backend: Option<Arc<dyn WorkspaceBackend>>,
    artifact_namespace: String,
    tool_result_retentions: BTreeMap<String, ToolResultRetention>,
    recovery_tool_available: bool,
}

#[derive(Debug)]
pub(crate) struct MemoryCompactionOutcome {
    pub(crate) messages: Vec<Message>,
    pub(crate) changed: bool,
    pub(crate) mode: MemoryCompactMode,
    pub(crate) archived_count: usize,
    pub(crate) reclaimed_tokens: u64,
    pub(crate) artifact_failure_count: usize,
}

struct CompactionRequest<'a> {
    cycle_index: u32,
    artifact_cycle_index: Option<u32>,
    force: bool,
    total_tokens: Option<u64>,
    recent_tool_call_ids: Option<&'a BTreeSet<String>>,
    microcompaction_plan: Option<microcompact::MicrocompactionPlan>,
}

impl fmt::Debug for MemoryManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemoryManager")
            .field("config", &self.config)
            .field("has_session_memory", &self.session_memory.is_some())
            .field("model_max_output_tokens", &self.model_max_output_tokens)
            .field("reserved_output_source", &self.reserved_output_source)
            .field("has_workspace_backend", &self.workspace_backend.is_some())
            .field("artifact_namespace", &self.artifact_namespace)
            .field("tool_result_retentions", &self.tool_result_retentions)
            .field("recovery_tool_available", &self.recovery_tool_available)
            .finish()
    }
}

impl MemoryManager {
    pub fn new(mut config: MemoryManagerConfig) -> Self {
        config
            .microcompaction_policy
            .validate()
            .expect("MemoryManagerConfig has an invalid microcompaction policy");
        let session_memory = config.session_memory.take();
        Self {
            config,
            session_memory,
            model_max_output_tokens: None,
            reserved_output_source: ReservedOutputSource::FrameworkFallback,
            runtime_callbacks: RuntimeMemoryCallbacks::default(),
            workspace_backend: None,
            artifact_namespace: "memory".to_string(),
            tool_result_retentions: BTreeMap::new(),
            recovery_tool_available: false,
        }
    }

    pub fn with_workspace_backend(mut self, backend: Arc<dyn WorkspaceBackend>) -> Self {
        self.workspace_backend = Some(backend);
        self
    }

    pub fn with_recovery_tool_available(mut self, available: bool) -> Self {
        self.recovery_tool_available = available;
        self
    }

    pub(crate) fn with_archive_context(
        mut self,
        artifact_namespace: impl Into<String>,
        tool_result_retentions: BTreeMap<String, ToolResultRetention>,
        recovery_tool_available: bool,
    ) -> Self {
        self.artifact_namespace = artifact_namespace.into();
        self.tool_result_retentions = tool_result_retentions;
        self.recovery_tool_available = recovery_tool_available;
        self
    }

    pub(crate) fn with_capacity_observation(
        mut self,
        model_max_output_tokens: Option<u64>,
        reserved_output_source: ReservedOutputSource,
    ) -> Self {
        self.model_max_output_tokens = model_max_output_tokens;
        self.reserved_output_source = reserved_output_source;
        self
    }

    pub(crate) fn with_runtime_callbacks(mut self, callbacks: RuntimeMemoryCallbacks) -> Self {
        self.runtime_callbacks = callbacks;
        self
    }

    pub(crate) fn model_max_output_tokens(&self) -> Option<u64> {
        self.model_max_output_tokens
    }

    pub(crate) fn reserved_output_source(&self) -> ReservedOutputSource {
        self.reserved_output_source
    }

    pub fn compact(&mut self, messages: &[Message], force: bool) -> (Vec<Message>, bool) {
        self.compact_for_cycle_with_usage_inner(
            messages,
            CompactionRequest {
                cycle_index: 0,
                artifact_cycle_index: None,
                force,
                total_tokens: None,
                recent_tool_call_ids: None,
                microcompaction_plan: None,
            },
        )
        .expect("public memory compaction has no runtime callback control flow")
        .into_tuple()
    }

    pub fn compact_for_cycle(
        &mut self,
        messages: &[Message],
        cycle_index: u32,
        force: bool,
    ) -> (Vec<Message>, bool) {
        self.compact_for_cycle_with_usage_inner(
            messages,
            CompactionRequest {
                cycle_index,
                artifact_cycle_index: Some(cycle_index),
                force,
                total_tokens: None,
                recent_tool_call_ids: None,
                microcompaction_plan: None,
            },
        )
        .expect("public memory compaction has no runtime callback control flow")
        .into_tuple()
    }

    pub fn compact_for_cycle_with_usage(
        &mut self,
        messages: &[Message],
        cycle_index: u32,
        force: bool,
        total_tokens: Option<u64>,
        recent_tool_call_ids: Option<&BTreeSet<String>>,
    ) -> (Vec<Message>, bool) {
        self.compact_for_cycle_with_usage_inner(
            messages,
            CompactionRequest {
                cycle_index,
                artifact_cycle_index: Some(cycle_index),
                force,
                total_tokens,
                recent_tool_call_ids,
                microcompaction_plan: None,
            },
        )
        .expect("public memory compaction has no runtime callback control flow")
        .into_tuple()
    }

    pub(crate) fn compact_for_cycle_with_usage_observed(
        &mut self,
        messages: &[Message],
        cycle_index: u32,
        force: bool,
        total_tokens: Option<u64>,
        recent_tool_call_ids: Option<&BTreeSet<String>>,
        microcompaction_plan: Option<microcompact::MicrocompactionPlan>,
    ) -> Result<MemoryCompactionOutcome, RuntimeMemoryCallbackError> {
        self.compact_for_cycle_with_usage_inner(
            messages,
            CompactionRequest {
                cycle_index,
                artifact_cycle_index: Some(cycle_index),
                force,
                total_tokens,
                recent_tool_call_ids,
                microcompaction_plan,
            },
        )
    }

    fn compact_for_cycle_with_usage_inner(
        &mut self,
        messages: &[Message],
        request: CompactionRequest<'_>,
    ) -> Result<MemoryCompactionOutcome, RuntimeMemoryCallbackError> {
        let CompactionRequest {
            cycle_index,
            artifact_cycle_index,
            force,
            total_tokens,
            recent_tool_call_ids,
            microcompaction_plan,
        } = request;
        if messages.is_empty() {
            return Ok(MemoryCompactionOutcome::new(
                messages,
                Vec::new(),
                MemoryCompactMode::None,
                false,
                microcompact::MicrocompactionApplication::default(),
            ));
        }

        let cleaned = self.remove_previous_summary(messages);
        let sanitized = filter_empty_assistant_messages(&cleaned);
        let changed_by_sanitize = sanitized != messages;
        let mut changed = changed_by_sanitize;
        let mut mode = if changed_by_sanitize {
            MemoryCompactMode::Structural
        } else {
            MemoryCompactMode::None
        };
        let mut working_messages = sanitized;
        let mut message_length =
            self.calculate_effective_length(&working_messages, total_tokens, recent_tool_call_ids);
        if let Some(session_memory) = self.session_memory.as_mut() {
            let text_messages = working_messages
                .iter()
                .filter(|message| {
                    !matches!(message.role, MessageRole::System | MessageRole::Tool)
                        && !message.content.trim().is_empty()
                })
                .count();
            let runtime_callback = self.runtime_callbacks.session_memory.as_ref();
            let should_extract = match runtime_callback {
                Some(_) => session_memory
                    .should_extract_with_runtime_callback(message_length, text_messages),
                None => session_memory.should_extract(message_length, text_messages),
            };
            if should_extract {
                let _ = match runtime_callback {
                    Some(callback) => session_memory.extract_with_runtime_callback(
                        &working_messages,
                        cycle_index as i32,
                        message_length,
                        callback,
                        self.runtime_callbacks.session_memory_diagnostic.as_ref(),
                    )?,
                    None => session_memory.extract(
                        &working_messages,
                        cycle_index as i32,
                        message_length,
                    ),
                };
            }
        }
        let mut microcompaction = microcompact::MicrocompactionApplication::default();
        if !force && self.should_preemptive_microcompact(message_length) {
            let plan = microcompaction_plan.unwrap_or_else(|| {
                self.plan_microcompaction(&working_messages, cycle_index, message_length)
            });
            microcompaction = self.apply_microcompaction(&working_messages, &plan);
            if microcompaction.archived_count > 0 {
                working_messages = microcompaction.messages.clone();
                mode = mode.max(MemoryCompactMode::Micro);
                changed = true;
                message_length = message_length.saturating_sub(microcompaction.reclaimed_tokens);
            }
        }
        if !force && message_length <= self.autocompact_threshold() {
            let (warned, warning_inserted) =
                self.maybe_append_memory_warning(&working_messages, message_length);
            if warning_inserted {
                mode = mode.max(MemoryCompactMode::Structural);
                changed = true;
            }
            return Ok(MemoryCompactionOutcome::new(
                messages,
                warned,
                mode,
                changed,
                microcompaction,
            ));
        }
        let mut summary_source = working_messages;
        if !force {
            let before_structural_tokens =
                count_messages_tokens(&summary_source, &self.config.model);
            let (image_compacted, image_changed) =
                compact_processed_image_messages(&summary_source);
            let (artifact_compacted, artifact_changed) =
                self.compact_large_tool_results(&image_compacted, artifact_cycle_index);
            let after_structural_tokens =
                count_messages_tokens(&artifact_compacted, &self.config.model);
            message_length = if after_structural_tokens >= before_structural_tokens {
                message_length.saturating_add(
                    after_structural_tokens.saturating_sub(before_structural_tokens),
                )
            } else {
                message_length.saturating_sub(
                    before_structural_tokens.saturating_sub(after_structural_tokens),
                )
            };
            if (image_changed || artifact_changed) && message_length <= self.autocompact_threshold()
            {
                return Ok(MemoryCompactionOutcome::new(
                    messages,
                    artifact_compacted,
                    mode.max(MemoryCompactMode::Structural),
                    true,
                    microcompaction,
                ));
            }
            if image_changed || artifact_changed {
                mode = mode.max(MemoryCompactMode::Structural);
                summary_source = artifact_compacted;
            }
        }
        let (compacted, summary_changed) = self.compress_memory(
            &summary_source,
            artifact_cycle_index,
            self.runtime_callbacks.memory_compaction.as_ref(),
        )?;
        if summary_changed {
            mode = mode.max(MemoryCompactMode::Summary);
            let post_compaction_tokens = count_messages_tokens(&compacted, &self.config.model);
            if let Some(session_memory) = self.session_memory.as_mut() {
                session_memory.on_compaction(Some(post_compaction_tokens));
            }
        }
        Ok(MemoryCompactionOutcome::new(
            messages,
            compacted,
            mode,
            changed || summary_changed,
            microcompaction,
        ))
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
}

impl MemoryCompactionOutcome {
    fn new(
        original: &[Message],
        messages: Vec<Message>,
        mode: MemoryCompactMode,
        changed: bool,
        microcompaction: microcompact::MicrocompactionApplication,
    ) -> Self {
        let content_changed = messages != original;
        let mode = if !content_changed {
            MemoryCompactMode::None
        } else if mode == MemoryCompactMode::None {
            MemoryCompactMode::Structural
        } else {
            mode
        };
        Self {
            messages,
            changed,
            mode,
            archived_count: microcompaction.archived_count,
            reclaimed_tokens: microcompaction.reclaimed_tokens,
            artifact_failure_count: microcompaction.artifact_failure_count,
        }
    }

    fn into_tuple(self) -> (Vec<Message>, bool) {
        (self.messages, self.changed)
    }
}
