use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::sessions::Session;
use crate::types::Metadata;

#[derive(Debug, Clone, PartialEq)]
pub struct ContextFragment {
    pub id: String,
    pub text: String,
    pub stable: bool,
    pub priority: i32,
    pub source: Option<String>,
    pub cache_hint: Option<String>,
    pub metadata: Metadata,
}

impl ContextFragment {
    pub fn new(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            stable: false,
            priority: 100,
            source: None,
            cache_hint: None,
            metadata: Metadata::new(),
        }
    }

    pub fn stable(mut self, stable: bool) -> Self {
        self.stable = stable;
        self
    }

    pub fn priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    pub fn cache_hint(mut self, cache_hint: impl Into<String>) -> Self {
        self.cache_hint = Some(cache_hint.into());
        self
    }

    pub fn metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }
}

pub struct ContextRequest<'a> {
    pub agent_name: &'a str,
    pub input: &'a str,
    pub model: Option<String>,
    pub trace_id: Option<String>,
    pub session: Option<Arc<dyn Session>>,
    pub workspace: Option<PathBuf>,
    pub context: Option<Arc<dyn std::any::Any + Send + Sync>>,
    pub max_prompt_chars: Option<usize>,
    pub metadata: Metadata,
}

impl<'a> ContextRequest<'a> {
    pub fn new(agent_name: &'a str, input: &'a str) -> Self {
        Self {
            agent_name,
            input,
            model: None,
            trace_id: None,
            session: None,
            workspace: None,
            context: None,
            max_prompt_chars: None,
            metadata: Metadata::new(),
        }
    }

    pub fn for_test(agent_name: &'a str, input: &'a str) -> Self {
        Self::new(agent_name, input)
    }

    pub fn max_prompt_chars(mut self, max_prompt_chars: usize) -> Self {
        self.max_prompt_chars = Some(max_prompt_chars);
        self
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn trace_id(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self
    }

    pub fn session(mut self, session: Arc<dyn Session>) -> Self {
        self.session = Some(session);
        self
    }

    pub fn workspace(mut self, workspace: impl Into<PathBuf>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    pub fn context(mut self, context: Arc<dyn std::any::Any + Send + Sync>) -> Self {
        self.context = Some(context);
        self
    }

    pub fn metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContextSection {
    pub id: String,
    pub text: String,
    pub stable: bool,
    pub priority: i32,
    pub source: Option<String>,
    pub cache_hint: Option<String>,
    pub metadata: Metadata,
}

impl ContextSection {
    pub fn to_metadata(&self) -> Value {
        let mut payload = serde_json::Map::from_iter([
            ("id".to_string(), Value::String(self.id.clone())),
            ("text".to_string(), Value::String(self.text.clone())),
            ("stable".to_string(), Value::Bool(self.stable)),
        ]);
        if let Some(source) = self.source.as_deref().filter(|value| !value.is_empty()) {
            payload.insert("source".to_string(), Value::String(source.to_string()));
        }
        if let Some(cache_hint) = &self.cache_hint {
            payload.insert("cache_hint".to_string(), Value::String(cache_hint.clone()));
        }
        if !self.metadata.is_empty() {
            payload.insert(
                "metadata".to_string(),
                serde_json::to_value(&self.metadata).unwrap_or(Value::Null),
            );
        }
        Value::Object(payload)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContextBundle {
    pub prompt: String,
    pub sections: Vec<ContextSection>,
    pub stable_hash: String,
    pub sources: BTreeMap<String, String>,
    pub total_chars: usize,
    pub omitted_section_ids: Vec<String>,
}

impl ContextBundle {
    pub fn metadata_sections(&self) -> Vec<Value> {
        self.sections
            .iter()
            .map(ContextSection::to_metadata)
            .collect()
    }
}

pub trait ContextProvider: Send + Sync {
    fn fragments(&self, request: &ContextRequest<'_>)
        -> Result<Vec<ContextFragment>, ContextError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextError {
    message: String,
}

impl ContextError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ContextError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ContextError {}

pub fn collect_context_fragments(
    request: &ContextRequest<'_>,
    providers: &[std::sync::Arc<dyn ContextProvider>],
) -> Result<Vec<ContextFragment>, ContextError> {
    let mut fragments = Vec::new();
    for provider in providers {
        fragments.extend(provider.fragments(request)?);
    }
    Ok(fragments)
}

pub fn assemble_context_fragments(
    request: &ContextRequest<'_>,
    mut fragments: Vec<ContextFragment>,
) -> Result<ContextBundle, ContextError> {
    fragments.sort_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| right.stable.cmp(&left.stable))
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut prompt_parts = Vec::new();
    let mut total_chars = 0usize;
    let mut sections = Vec::new();
    let mut stable_parts = Vec::new();
    let mut sources = BTreeMap::new();
    let mut omitted_section_ids = Vec::new();

    for fragment in fragments {
        let text = fragment.text.trim().to_string();
        if text.is_empty() {
            continue;
        }
        let separator_chars = usize::from(!prompt_parts.is_empty()) * 2;
        let candidate_len = total_chars + separator_chars + text.chars().count();
        if request
            .max_prompt_chars
            .is_some_and(|max_chars| candidate_len > max_chars)
        {
            omitted_section_ids.push(fragment.id);
            continue;
        }
        if let Some(source) = fragment.source.as_ref() {
            sources.insert(fragment.id.clone(), source.clone());
        }
        if fragment.stable {
            stable_parts.push(text.clone());
        }
        prompt_parts.push(text.clone());
        total_chars = candidate_len;
        sections.push(ContextSection {
            id: fragment.id,
            text,
            stable: fragment.stable,
            priority: fragment.priority,
            source: fragment.source,
            cache_hint: fragment.cache_hint,
            metadata: fragment.metadata,
        });
    }

    Ok(ContextBundle {
        prompt: prompt_parts.join("\n\n"),
        sections,
        stable_hash: sha256_hex(stable_parts.join("").as_bytes()),
        sources,
        total_chars,
        omitted_section_ids,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_lower(&hasher.finalize())
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
