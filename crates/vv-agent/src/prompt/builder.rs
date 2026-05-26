use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{SecondsFormat, Utc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::templates;

#[derive(Clone)]
pub struct PromptSection {
    pub id: String,
    stable: bool,
    compute: Arc<dyn Fn() -> String + Send + Sync>,
    cached_value: Arc<Mutex<Option<String>>>,
}

impl PromptSection {
    pub fn new(
        id: impl Into<String>,
        compute: impl Fn() -> String + Send + Sync + 'static,
        stable: bool,
    ) -> Self {
        Self {
            id: id.into(),
            stable,
            compute: Arc::new(compute),
            cached_value: Arc::new(Mutex::new(None)),
        }
    }

    pub fn constant(id: impl Into<String>, text: impl Into<String>, stable: bool) -> Self {
        let text = text.into();
        Self::new(id, move || text.clone(), stable)
    }

    pub fn get_value(&self) -> String {
        if self.stable {
            let mut cached = self.cached_value.lock().expect("prompt section cache");
            if let Some(value) = cached.as_ref() {
                return value.clone();
            }
            let value = (self.compute)();
            *cached = Some(value.clone());
            return value;
        }
        (self.compute)()
    }

    pub fn invalidate(&self) {
        let mut cached = self.cached_value.lock().expect("prompt section cache");
        *cached = None;
    }

    pub fn to_metadata(&self) -> Option<Value> {
        let text = self.get_value().trim().to_string();
        if text.is_empty() {
            return None;
        }
        Some(json!({
            "id": self.id,
            "text": text,
            "stable": self.stable,
        }))
    }

    pub fn stable(&self) -> bool {
        self.stable
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltSystemPrompt {
    pub prompt: String,
    pub sections: Vec<Value>,
    pub stable_hash: String,
}

#[derive(Clone)]
pub struct BuildSystemPromptOptions {
    pub language: String,
    pub allow_interruption: bool,
    pub use_workspace: bool,
    pub enable_todo_management: bool,
    pub agent_type: Option<String>,
    pub available_sub_agents: BTreeMap<String, String>,
    pub available_skills: Option<Value>,
    pub workspace: Option<PathBuf>,
    pub current_time_utc: Option<String>,
    pub session_memory_context: String,
}

impl Default for BuildSystemPromptOptions {
    fn default() -> Self {
        Self {
            language: "en-US".to_string(),
            allow_interruption: true,
            use_workspace: true,
            enable_todo_management: true,
            agent_type: None,
            available_sub_agents: BTreeMap::new(),
            available_skills: None,
            workspace: None,
            current_time_utc: None,
            session_memory_context: String::new(),
        }
    }
}

#[derive(Clone, Default)]
pub struct SystemPromptBuilder {
    sections: Vec<PromptSection>,
}

impl SystemPromptBuilder {
    pub fn add_section(&mut self, section: PromptSection) {
        self.sections.push(section);
    }

    pub fn build(&self) -> String {
        self.sections
            .iter()
            .filter_map(|section| {
                let value = section.get_value().trim().to_string();
                (!value.is_empty()).then_some(value)
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn metadata_sections(&self) -> Vec<Value> {
        self.sections
            .iter()
            .filter_map(PromptSection::to_metadata)
            .collect()
    }

    pub fn invalidate_all(&self) {
        for section in &self.sections {
            section.invalidate();
        }
    }

    pub fn invalidate_volatile(&self) {
        for section in &self.sections {
            if !section.stable() {
                section.invalidate();
            }
        }
    }

    pub fn stable_hash(&self) -> String {
        let stable_text = self
            .sections
            .iter()
            .filter(|section| section.stable())
            .map(|section| section.get_value().trim().to_string())
            .collect::<String>();
        sha256_hex(stable_text.as_bytes())
    }

    pub fn build_result(&self) -> BuiltSystemPrompt {
        let mut prompt_parts = Vec::new();
        let mut sections = Vec::new();
        let mut stable_parts = Vec::new();
        for section in &self.sections {
            let value = section.get_value().trim().to_string();
            if value.is_empty() {
                continue;
            }
            prompt_parts.push(value.clone());
            sections.push(json!({
                "id": section.id,
                "text": value,
                "stable": section.stable(),
            }));
            if section.stable() {
                stable_parts.push(value);
            }
        }
        BuiltSystemPrompt {
            prompt: prompt_parts.join("\n\n"),
            sections,
            stable_hash: sha256_hex(stable_parts.join("").as_bytes()),
        }
    }
}

pub fn build_system_prompt(original_system_prompt: impl Into<String>) -> String {
    build_system_prompt_bundle(original_system_prompt).prompt
}

pub fn build_system_prompt_with_options(
    original_system_prompt: impl Into<String>,
    options: BuildSystemPromptOptions,
) -> String {
    build_system_prompt_bundle_with_options(original_system_prompt, options).prompt
}

pub fn build_system_prompt_sections(original_system_prompt: impl Into<String>) -> Vec<Value> {
    build_system_prompt_bundle(original_system_prompt).sections
}

pub fn build_system_prompt_sections_with_options(
    original_system_prompt: impl Into<String>,
    options: BuildSystemPromptOptions,
) -> Vec<Value> {
    build_system_prompt_bundle_with_options(original_system_prompt, options).sections
}

pub fn build_system_prompt_bundle(original_system_prompt: impl Into<String>) -> BuiltSystemPrompt {
    build_system_prompt_bundle_with_options(
        original_system_prompt,
        BuildSystemPromptOptions::default(),
    )
}

pub fn build_system_prompt_bundle_with_options(
    original_system_prompt: impl Into<String>,
    options: BuildSystemPromptOptions,
) -> BuiltSystemPrompt {
    create_system_prompt_builder(original_system_prompt, options).build_result()
}

pub fn create_system_prompt_builder(
    original_system_prompt: impl Into<String>,
    options: BuildSystemPromptOptions,
) -> SystemPromptBuilder {
    let language = options.language;
    let mut builder = SystemPromptBuilder::default();
    builder.add_section(PromptSection::constant(
        "agent_definition",
        format!(
            "<Agent Definition>\n{}\n</Agent Definition>",
            original_system_prompt.into()
        ),
        true,
    ));

    if !options.session_memory_context.trim().is_empty() {
        builder.add_section(PromptSection::constant(
            "session_memory",
            options.session_memory_context,
            false,
        ));
    }

    if options.agent_type.as_deref() == Some("computer") {
        builder.add_section(PromptSection::constant(
            "environment",
            format!(
                "<Environment>\n{}\n</Environment>",
                templates::computer_agent_env_prompt(&language)
            ),
            true,
        ));
    }

    let mut tools_lines = Vec::new();
    if options.allow_interruption {
        tools_lines.push(templates::ask_user_prompt(&language).to_string());
    }
    if options.use_workspace {
        tools_lines.push(templates::render_workspace_tools(&language));
        tools_lines.push(templates::tool_priority_prompt(&language).to_string());
    }
    if options.enable_todo_management {
        tools_lines.push(templates::todo_prompt(&language).to_string());
    }
    if !options.available_sub_agents.is_empty() {
        tools_lines.push(templates::render_sub_agents(
            &language,
            &options.available_sub_agents,
        ));
    }
    if let Some(available_skills) = options.available_skills.as_ref() {
        tools_lines.push(templates::render_available_skills(
            &language,
            available_skills,
            options.workspace.as_deref(),
        ));
    }
    tools_lines.push(templates::task_finish_prompt(&language).to_string());
    builder.add_section(PromptSection::constant(
        "tools",
        format!("<Tools>\n{}\n</Tools>", tools_lines.join("\n\n")),
        true,
    ));

    let current_time = options.current_time_utc.unwrap_or_else(current_utc_text);
    builder.add_section(PromptSection::constant(
        "current_time",
        format!(
            "<Current Time>\n{}\n{}\n</Current Time>",
            templates::current_time_prompt(&language),
            current_time
        ),
        false,
    ));
    builder
}

pub fn build_raw_system_prompt_sections(system_prompt: impl Into<String>) -> Vec<Value> {
    let text = system_prompt.into().trim().to_string();
    if text.is_empty() {
        return Vec::new();
    }
    vec![json!({
        "id": "raw_system_prompt",
        "text": text,
        "stable": true,
    })]
}

fn current_utc_text() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex_lower(&digest)
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}
