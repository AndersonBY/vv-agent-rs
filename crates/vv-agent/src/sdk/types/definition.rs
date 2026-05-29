use std::collections::BTreeMap;

use crate::types::{Metadata, NoToolPolicy, SubAgentConfig};

#[derive(Debug, Clone, PartialEq)]
pub struct AgentDefinition {
    pub description: String,
    pub model: String,
    pub backend: Option<String>,
    pub language: String,
    pub max_cycles: u32,
    pub memory_compact_threshold: u64,
    pub memory_threshold_percentage: u8,
    pub no_tool_policy: NoToolPolicy,
    pub allow_interruption: bool,
    pub use_workspace: bool,
    pub enable_todo_management: bool,
    pub agent_type: Option<String>,
    pub native_multimodal: bool,
    pub enable_sub_agents: bool,
    pub sub_agents: BTreeMap<String, SubAgentConfig>,
    pub skill_directories: Vec<String>,
    pub extra_tool_names: Vec<String>,
    pub exclude_tools: Vec<String>,
    pub bash_shell: Option<String>,
    pub windows_shell_priority: Vec<String>,
    pub bash_env: BTreeMap<String, String>,
    pub metadata: Metadata,
    pub system_prompt: Option<String>,
    pub system_prompt_template: Option<String>,
}

impl AgentDefinition {
    pub fn default_for_model(model: impl Into<String>) -> Self {
        Self {
            description: "General-purpose VectorVein agent profile".to_string(),
            model: model.into(),
            backend: None,
            language: "zh-CN".to_string(),
            max_cycles: 10,
            memory_compact_threshold: 128_000,
            memory_threshold_percentage: 90,
            no_tool_policy: NoToolPolicy::Continue,
            allow_interruption: true,
            use_workspace: true,
            enable_todo_management: true,
            agent_type: None,
            native_multimodal: false,
            enable_sub_agents: false,
            sub_agents: BTreeMap::new(),
            skill_directories: Vec::new(),
            extra_tool_names: Vec::new(),
            exclude_tools: Vec::new(),
            bash_shell: None,
            windows_shell_priority: Vec::new(),
            bash_env: BTreeMap::new(),
            metadata: Metadata::new(),
            system_prompt: None,
            system_prompt_template: None,
        }
    }
}
