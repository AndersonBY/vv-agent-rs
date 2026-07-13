use serde::Serialize;

use crate::types::Message;

mod events;
mod files;
mod original;
mod text;

use events::{build_progress_events, collect_errors, current_work_state};
use files::collect_file_actions;
use original::collect_original_user_messages;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileAction {
    pub path: String,
    pub action: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorFix {
    pub error: String,
    pub fix: String,
    pub file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalSummary {
    pub summary_version: String,
    pub original_user_messages: Vec<String>,
    pub user_constraints: Vec<String>,
    pub decisions: Vec<String>,
    pub files_examined_or_modified: Vec<FileAction>,
    pub errors_and_fixes: Vec<ErrorFix>,
    pub progress: Vec<String>,
    pub key_facts: Vec<String>,
    pub open_issues: Vec<String>,
    pub current_work_state: String,
    pub next_steps: Vec<String>,
}

impl LocalSummary {
    pub fn from_messages(messages: &[Message], event_limit: usize) -> Self {
        Self::from_messages_with_key_facts(messages, event_limit, Vec::new())
    }

    pub fn summarize_content(content: &str, limit: usize) -> String {
        text::normalize_excerpt(content, limit)
    }

    pub(crate) fn from_messages_with_key_facts(
        messages: &[Message],
        event_limit: usize,
        key_facts: Vec<String>,
    ) -> Self {
        Self {
            summary_version: "2.0".to_string(),
            original_user_messages: collect_original_user_messages(messages),
            user_constraints: Vec::new(),
            decisions: Vec::new(),
            files_examined_or_modified: collect_file_actions(messages),
            errors_and_fixes: collect_errors(messages),
            progress: build_progress_events(messages, event_limit),
            key_facts,
            open_issues: Vec::new(),
            current_work_state: current_work_state(messages),
            next_steps: Vec::new(),
        }
    }

    pub fn to_json_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}
