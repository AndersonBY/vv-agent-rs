use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::memory::token_utils::estimate_tokens;
use crate::types::{Message, MessageRole};

const DEFAULT_MIN_TOKENS: u64 = 10_000;
const DEFAULT_MAX_TOKENS: u64 = 40_000;
const DEFAULT_MIN_TEXT_MESSAGES: usize = 5;
const DEFAULT_GROWTH_RATIO: f64 = 0.5;
const SESSION_MEMORY_FILENAME: &str = "session_memory.json";
const SESSION_MEMORY_CATEGORIES: &[&str] = &[
    "user_intent",
    "decision",
    "file_change",
    "error_fix",
    "key_fact",
];

pub type SessionMemoryExtractionCallback =
    Arc<dyn Fn(&str, Option<&str>, Option<&str>) -> Option<String> + Send + Sync + 'static>;

#[derive(Clone)]
pub struct SessionMemoryConfig {
    pub min_tokens_before_extraction: u64,
    pub max_tokens: u64,
    pub min_text_messages: usize,
    pub growth_ratio: f64,
    pub storage_dir: PathBuf,
    pub extraction_callback: Option<SessionMemoryExtractionCallback>,
    pub extraction_backend: Option<String>,
    pub extraction_model: Option<String>,
    pub token_model: String,
}

impl std::fmt::Debug for SessionMemoryConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionMemoryConfig")
            .field(
                "min_tokens_before_extraction",
                &self.min_tokens_before_extraction,
            )
            .field("max_tokens", &self.max_tokens)
            .field("min_text_messages", &self.min_text_messages)
            .field("growth_ratio", &self.growth_ratio)
            .field("storage_dir", &self.storage_dir)
            .field(
                "extraction_callback",
                &self.extraction_callback.as_ref().map(|_| "<callback>"),
            )
            .field("extraction_backend", &self.extraction_backend)
            .field("extraction_model", &self.extraction_model)
            .field("token_model", &self.token_model)
            .finish()
    }
}

impl Default for SessionMemoryConfig {
    fn default() -> Self {
        Self {
            min_tokens_before_extraction: DEFAULT_MIN_TOKENS,
            max_tokens: DEFAULT_MAX_TOKENS,
            min_text_messages: DEFAULT_MIN_TEXT_MESSAGES,
            growth_ratio: DEFAULT_GROWTH_RATIO,
            storage_dir: PathBuf::from(".memory/session"),
            extraction_callback: None,
            extraction_backend: None,
            extraction_model: None,
            token_model: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMemoryEntry {
    pub category: String,
    pub content: String,
    pub source_cycle: i32,
    pub importance: u8,
}

impl SessionMemoryEntry {
    pub fn new(
        category: impl Into<String>,
        content: impl Into<String>,
        source_cycle: i32,
        importance: u8,
    ) -> Self {
        Self::normalized(category.into(), content.into(), source_cycle, importance)
    }

    fn normalized(category: String, content: String, source_cycle: i32, importance: u8) -> Self {
        let normalized_category = normalize_category(&category);
        Self {
            category: normalized_category,
            content: content.trim().to_string(),
            source_cycle,
            importance: importance.clamp(1, 10),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMemoryState {
    pub entries: Vec<SessionMemoryEntry>,
    pub last_extracted_message_index: i32,
    pub tokens_at_last_extraction: u64,
    pub initialized: bool,
}

impl Default for SessionMemoryState {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            last_extracted_message_index: -1,
            tokens_at_last_extraction: 0,
            initialized: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionMemory {
    pub config: SessionMemoryConfig,
    pub state: SessionMemoryState,
    workspace: Option<PathBuf>,
    storage_scope: Option<String>,
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
        if self.config.extraction_callback.is_none() || current_tokens == 0 || message_count == 0 {
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

        let prompt = self.build_extraction_prompt(&new_messages);
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

    pub fn storage_path(&self) -> Option<PathBuf> {
        let workspace = self.workspace.as_ref()?;
        let workspace_root = workspace.canonicalize().ok()?;
        let storage_dir = if self.config.storage_dir.is_absolute() {
            self.config.storage_dir.clone()
        } else {
            workspace.join(&self.config.storage_dir)
        };
        let scoped_dir = match self
            .storage_scope
            .as_deref()
            .filter(|scope| !scope.is_empty())
        {
            Some(scope) => storage_dir.join(sanitize_storage_scope(scope)?),
            None => storage_dir,
        };
        let resolved = scoped_dir.join(SESSION_MEMORY_FILENAME);
        let parent = resolved.parent()?;
        let canonical_parent = if parent.exists() {
            parent.canonicalize().ok()?
        } else {
            canonicalize_existing_ancestor(parent)?
        };
        if !canonical_parent.starts_with(&workspace_root) {
            return None;
        }
        Some(resolved)
    }

    pub fn parse_extraction_result(&self, raw: &str, cycle: i32) -> Vec<SessionMemoryEntry> {
        let Some(array_text) = extract_first_json_array(raw) else {
            return Vec::new();
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(array_text) else {
            return Vec::new();
        };
        let Some(items) = value.as_array() else {
            return Vec::new();
        };
        items
            .iter()
            .filter_map(|item| {
                let object = item.as_object()?;
                let content = object.get("content")?.as_str()?.trim();
                if content.is_empty() {
                    return None;
                }
                let category = object
                    .get("category")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("key_fact");
                let importance = object
                    .get("importance")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(5)
                    .min(10) as u8;
                Some(SessionMemoryEntry::new(
                    category,
                    content,
                    cycle,
                    importance.max(1),
                ))
            })
            .collect()
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

    fn build_extraction_prompt(&self, messages: &[&Message]) -> String {
        let serialized_messages = messages
            .iter()
            .map(|message| message_to_text(message))
            .collect::<Vec<_>>();
        format!(
            "Analyze the following conversation messages and extract durable facts that should survive context compression.\n\n\
Categories:\n\
- user_intent: goals, constraints, preferences, explicit asks\n\
- decision: decisions or chosen approaches\n\
- file_change: files created/modified/deleted and why\n\
- error_fix: failures and their resolutions\n\
- key_fact: other important context that should not be forgotten\n\n\
Requirements:\n\
- Return JSON array only.\n\
- Keep each content field concise and deduplicatable.\n\
- Skip transient chatter and repeated information.\n\
- importance is 1-10 where 10 means critical.\n\n\
Output format:\n\
[{example}]\n\n\
Messages:\n{}",
            serde_json::to_string_pretty(&serialized_messages).unwrap_or_default(),
            example = r#"{"category":"...", "content":"...", "importance": 5}"#
        )
    }

    fn record_extraction(&mut self, last_message_index: i32, current_tokens: u64) {
        self.state.last_extracted_message_index = last_message_index;
        self.state.tokens_at_last_extraction = current_tokens;
        self.state.initialized = true;
    }
}

fn should_skip_message(message: &Message) -> bool {
    message.role == MessageRole::System
        || (message.role == MessageRole::User
            && message.content.contains("<Compressed Agent Memory>"))
}

fn message_to_text(message: &Message) -> serde_json::Value {
    let role = match message.role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    };
    let mut object = serde_json::Map::new();
    object.insert("role".to_string(), serde_json::json!(role));
    object.insert(
        "content".to_string(),
        serde_json::json!(compact_long_content(&message.content)),
    );
    if let Some(name) = &message.name {
        object.insert("name".to_string(), serde_json::json!(name));
    }
    if let Some(tool_call_id) = &message.tool_call_id {
        object.insert("tool_call_id".to_string(), serde_json::json!(tool_call_id));
    }
    if !message.tool_calls.is_empty() {
        object.insert(
            "tool_calls".to_string(),
            serde_json::to_value(&message.tool_calls).unwrap_or(serde_json::Value::Null),
        );
    }
    serde_json::Value::Object(object)
}

fn compact_long_content(content: &str) -> String {
    if content.len() <= 2_000 {
        return content.to_string();
    }
    let head = content.chars().take(1_200).collect::<String>();
    let tail = content
        .chars()
        .rev()
        .take(400)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("{head}\n...[truncated]...\n{tail}")
}

fn normalize_category(category: &str) -> String {
    let category = category.trim().to_ascii_lowercase();
    if SESSION_MEMORY_CATEGORIES.contains(&category.as_str()) {
        category
    } else {
        "key_fact".to_string()
    }
}

fn entry_key(entry: &SessionMemoryEntry) -> (String, String) {
    (
        entry.category.clone(),
        entry
            .content
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase(),
    )
}

fn sanitize_storage_scope(raw: &str) -> Option<String> {
    let safe = raw
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches(['.', '_', '-'])
        .to_string();
    (!safe.is_empty()).then_some(safe)
}

fn canonicalize_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut current = path;
    while !current.exists() {
        current = current.parent()?;
    }
    current.canonicalize().ok()
}

fn extract_first_json_array(raw: &str) -> Option<&str> {
    let start = raw.find('[')?;
    let mut depth = 0_i32;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in raw[start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '[' if !in_string => depth += 1,
            ']' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(&raw[start..start + offset + ch.len_utf8()]);
                }
            }
            _ => {}
        }
    }
    None
}
