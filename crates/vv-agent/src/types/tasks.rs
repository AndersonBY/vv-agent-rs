use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{json_value_from_serializable, AgentStatus, Message, Metadata, NoToolPolicy};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubAgentConfig {
    pub model: String,
    pub description: String,
    pub backend: Option<String>,
    pub system_prompt: Option<String>,
    pub max_cycles: u32,
    pub exclude_tools: Vec<String>,
    pub metadata: Metadata,
}

impl SubAgentConfig {
    pub fn new(model: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            description: description.into(),
            backend: None,
            system_prompt: None,
            max_cycles: 8,
            exclude_tools: Vec::new(),
            metadata: Metadata::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentTask {
    pub task_id: String,
    pub model: String,
    pub system_prompt: String,
    pub user_prompt: String,
    pub max_cycles: u32,
    pub memory_compact_threshold: u64,
    pub memory_threshold_percentage: u8,
    pub no_tool_policy: NoToolPolicy,
    pub allow_interruption: bool,
    pub use_workspace: bool,
    pub has_sub_agents: bool,
    pub sub_agents: BTreeMap<String, SubAgentConfig>,
    pub agent_type: Option<String>,
    pub native_multimodal: bool,
    pub extra_tool_names: Vec<String>,
    pub exclude_tools: Vec<String>,
    pub initial_messages: Vec<Message>,
    pub initial_shared_state: Metadata,
    pub metadata: Metadata,
}

impl AgentTask {
    pub fn new(
        task_id: impl Into<String>,
        model: impl Into<String>,
        system_prompt: impl Into<String>,
        user_prompt: impl Into<String>,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            model: model.into(),
            system_prompt: system_prompt.into(),
            user_prompt: user_prompt.into(),
            max_cycles: 8,
            memory_compact_threshold: 128_000,
            memory_threshold_percentage: 90,
            no_tool_policy: NoToolPolicy::Continue,
            allow_interruption: true,
            use_workspace: true,
            has_sub_agents: false,
            sub_agents: BTreeMap::new(),
            agent_type: None,
            native_multimodal: false,
            extra_tool_names: Vec::new(),
            exclude_tools: Vec::new(),
            initial_messages: Vec::new(),
            initial_shared_state: Metadata::new(),
            metadata: Metadata::new(),
        }
    }

    pub fn sub_agents_enabled(&self) -> bool {
        self.has_sub_agents || !self.sub_agents.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubTaskRequest {
    pub agent_name: String,
    pub task_description: String,
    pub output_requirements: String,
    pub include_main_summary: bool,
    pub exclude_files_pattern: Option<String>,
    pub metadata: Metadata,
}

impl SubTaskRequest {
    pub fn new(agent_name: impl Into<String>, task_description: impl Into<String>) -> Self {
        Self {
            agent_name: agent_name.into(),
            task_description: task_description.into(),
            output_requirements: String::new(),
            include_main_summary: false,
            exclude_files_pattern: None,
            metadata: Metadata::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubTaskOutcome {
    pub task_id: String,
    pub agent_name: String,
    pub status: AgentStatus,
    pub session_id: Option<String>,
    pub final_answer: Option<String>,
    pub wait_reason: Option<String>,
    pub error: Option<String>,
    pub cycles: u32,
    pub todo_list: Vec<Value>,
    pub resolved: BTreeMap<String, String>,
}

impl SubTaskOutcome {
    pub fn to_dict(&self) -> Value {
        self.to_value()
    }

    pub fn to_value(&self) -> Value {
        json_value_from_serializable(self)
    }
}
