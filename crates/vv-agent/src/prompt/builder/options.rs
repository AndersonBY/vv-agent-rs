use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

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
