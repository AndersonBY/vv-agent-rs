mod config;
mod entry;
mod parse;
mod prompt;
mod state;
mod storage;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

use crate::memory::token_utils::estimate_tokens;
use crate::memory::{
    RuntimeMemoryCallback, RuntimeMemoryCallbackError, SessionMemoryDiagnosticCallback,
    SessionMemoryOutputDiagnostic,
};
use crate::types::Message;

pub use config::{SessionMemoryConfig, SessionMemoryExtractionCallback};
pub use entry::SessionMemoryEntry;
use entry::{entry_key, SESSION_MEMORY_CATEGORIES};
use prompt::{build_extraction_prompt, should_skip_message};
pub use state::SessionMemoryState;

#[derive(Debug, Clone)]
pub struct SessionMemory {
    pub config: SessionMemoryConfig,
    pub state: SessionMemoryState,
    pub(super) workspace: Option<PathBuf>,
    pub(super) storage_scope: Option<String>,
}

impl SessionMemory {
    pub fn new(config: SessionMemoryConfig) -> Self {
        Self::with_workspace(config, None, None)
    }

    pub fn with_workspace(
        config: SessionMemoryConfig,
        workspace: Option<PathBuf>,
        storage_scope: Option<String>,
    ) -> Self {
        Self {
            config,
            state: SessionMemoryState::default(),
            workspace,
            storage_scope: storage_scope.map(|scope| scope.trim().to_string()),
        }
    }

    pub fn should_extract(&self, current_tokens: u64, message_count: usize) -> bool {
        if self.config.extraction_callback.is_none() {
            return false;
        }
        self.should_extract_with_runtime_callback(current_tokens, message_count)
    }

    pub(crate) fn should_extract_with_runtime_callback(
        &self,
        current_tokens: u64,
        message_count: usize,
    ) -> bool {
        if current_tokens == 0 || message_count == 0 {
            return false;
        }

        if !self.state.initialized {
            return current_tokens >= self.config.min_tokens_before_extraction
                && message_count >= self.config.min_text_messages;
        }

        let growth_threshold = ((self.config.min_tokens_before_extraction as f64)
            * self.config.growth_ratio)
            .floor()
            .max(1.0) as u64;
        let growth = if current_tokens >= self.state.tokens_at_last_extraction {
            current_tokens - self.state.tokens_at_last_extraction
        } else {
            current_tokens
        };
        growth >= growth_threshold
    }

    pub fn extract(
        &mut self,
        messages: &[Message],
        current_cycle: i32,
        current_tokens: u64,
    ) -> usize {
        let Some(callback) = self.config.extraction_callback.as_ref().cloned() else {
            return 0;
        };
        if messages.is_empty() {
            return 0;
        }

        let start_index = if self.state.last_extracted_message_index >= 0
            && (self.state.last_extracted_message_index as usize) < messages.len()
        {
            self.state.last_extracted_message_index as usize + 1
        } else {
            0
        };
        let new_messages = messages
            .iter()
            .enumerate()
            .filter_map(|(index, message)| {
                (index >= start_index && !should_skip_message(message)).then_some(message)
            })
            .collect::<Vec<_>>();

        if new_messages.is_empty() {
            self.record_extraction(messages.len() as i32 - 1, current_tokens);
            return 0;
        }

        let prompt = build_extraction_prompt(&new_messages);
        let raw_result = catch_unwind(AssertUnwindSafe(|| {
            callback(
                &prompt,
                self.config.extraction_backend.as_deref(),
                self.config.extraction_model.as_deref(),
            )
        }));
        let Some(raw_result) = raw_result.ok().flatten() else {
            return 0;
        };

        let entries = self.parse_extraction_result(&raw_result, current_cycle);
        let merged_count = self.merge_entries(entries);
        self.prune_to_budget();
        self.record_extraction(messages.len() as i32 - 1, current_tokens);
        self.save();
        merged_count
    }

    pub(crate) fn extract_with_runtime_callback(
        &mut self,
        messages: &[Message],
        current_cycle: i32,
        current_tokens: u64,
        callback: &RuntimeMemoryCallback,
        diagnostic_callback: Option<&SessionMemoryDiagnosticCallback>,
    ) -> Result<usize, RuntimeMemoryCallbackError> {
        if messages.is_empty() {
            return Ok(0);
        }
        let new_messages = self.new_extraction_messages(messages);
        if new_messages.is_empty() {
            self.record_extraction(messages.len() as i32 - 1, current_tokens);
            return Ok(0);
        }

        let prompt = build_extraction_prompt(&new_messages);
        let Some(raw_result) = callback(
            &prompt,
            self.config.extraction_backend.as_deref(),
            self.config.extraction_model.as_deref(),
            current_cycle.max(1) as u32,
        )?
        else {
            return Ok(0);
        };
        let entries = match self.parse_extraction_result_checked(&raw_result, current_cycle) {
            Ok(entries) => entries,
            Err(reason) => {
                if let Some(diagnostic_callback) = diagnostic_callback {
                    diagnostic_callback(&SessionMemoryOutputDiagnostic {
                        cycle_index: current_cycle.max(1) as u32,
                        backend: self.config.extraction_backend.clone(),
                        model: self.config.extraction_model.clone(),
                        reason,
                    });
                }
                return Ok(0);
            }
        };
        let merged_count = self.merge_entries(entries);
        self.prune_to_budget();
        self.record_extraction(messages.len() as i32 - 1, current_tokens);
        self.save();
        Ok(merged_count)
    }

    fn new_extraction_messages<'a>(&self, messages: &'a [Message]) -> Vec<&'a Message> {
        let start_index = if self.state.last_extracted_message_index >= 0
            && (self.state.last_extracted_message_index as usize) < messages.len()
        {
            self.state.last_extracted_message_index as usize + 1
        } else {
            0
        };
        messages
            .iter()
            .enumerate()
            .filter_map(|(index, message)| {
                (index >= start_index && !should_skip_message(message)).then_some(message)
            })
            .collect()
    }

    pub fn render_as_system_context(&self) -> String {
        if self.state.entries.is_empty() {
            return String::new();
        }

        let mut parts = vec!["<Session Memory>".to_string()];
        for category in SESSION_MEMORY_CATEGORIES {
            let entries = self
                .state
                .entries
                .iter()
                .filter(|entry| entry.category == *category)
                .collect::<Vec<_>>();
            if entries.is_empty() {
                continue;
            }
            parts.push(format!("## {category}"));
            for entry in entries {
                parts.push(format!("- {}", entry.content));
            }
        }
        parts.push("</Session Memory>".to_string());
        parts.join("\n")
    }

    pub fn on_compaction(&mut self, current_tokens: Option<u64>) {
        self.state.last_extracted_message_index = -1;
        if let Some(current_tokens) = current_tokens {
            self.state.tokens_at_last_extraction = current_tokens;
            self.state.initialized = true;
        }
        self.save();
    }

    pub fn load(&mut self) {
        let Some(path) = self.storage_path() else {
            return;
        };
        let Ok(content) = std::fs::read_to_string(path) else {
            return;
        };
        let Ok(state) = serde_json::from_str::<SessionMemoryState>(&content) else {
            return;
        };
        self.state = state;
    }

    pub fn save(&self) {
        let Some(path) = self.storage_path() else {
            return;
        };
        let Some(parent) = path.parent() else {
            return;
        };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let Ok(content) = serde_json::to_string_pretty(&self.state) else {
            return;
        };
        let _ = std::fs::write(path, content);
    }

    pub fn merge_entries(&mut self, entries: Vec<SessionMemoryEntry>) -> usize {
        let mut merged = 0;
        for entry in entries {
            let key = entry_key(&entry);
            if let Some(existing) = self
                .state
                .entries
                .iter_mut()
                .find(|candidate| entry_key(candidate) == key)
            {
                existing.importance = existing.importance.max(entry.importance);
                existing.source_cycle = existing.source_cycle.max(entry.source_cycle);
                continue;
            }
            self.state.entries.push(entry);
            merged += 1;
        }
        merged
    }

    pub fn prune_to_budget(&mut self) {
        if self.config.max_tokens == 0 || self.state.entries.is_empty() {
            return;
        }
        let mut current_tokens =
            estimate_tokens(&self.render_as_system_context(), &self.config.token_model);
        while current_tokens > self.config.max_tokens && !self.state.entries.is_empty() {
            let Some((drop_index, _)) = self
                .state
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(index, entry)| (entry.importance, entry.source_cycle, *index as i32))
            else {
                break;
            };
            self.state.entries.remove(drop_index);
            current_tokens =
                estimate_tokens(&self.render_as_system_context(), &self.config.token_model);
        }
    }

    fn record_extraction(&mut self, last_message_index: i32, current_tokens: u64) {
        self.state.last_extracted_message_index = last_message_index;
        self.state.tokens_at_last_extraction = current_tokens;
        self.state.initialized = true;
    }
}
