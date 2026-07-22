use std::collections::BTreeMap;

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::model_settings::ModelSettings;
use crate::tools::common::trim_portable_whitespace;
use crate::tools::{ToolPolicy, ToolSideEffect};

use super::{
    json_value_from_serializable, AgentStatus, CompletionReason, Message, Metadata, NoToolPolicy,
};

pub const INVALID_SUB_AGENT_MODEL_CODE: &str = "invalid_sub_agent_model";
pub const INVALID_SUB_AGENT_MODEL_MESSAGE: &str = "sub-agent model cannot be empty";
pub const INVALID_SUB_AGENT_SYSTEM_PROMPT_CODE: &str = "invalid_sub_agent_system_prompt";
pub const INVALID_SUB_AGENT_SYSTEM_PROMPT_MESSAGE: &str =
    "sub-agent system_prompt cannot be empty when provided";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubAgentConfigValidationError {
    code: &'static str,
    message: &'static str,
}

impl SubAgentConfigValidationError {
    fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn message(&self) -> &'static str {
        self.message
    }
}

impl std::fmt::Display for SubAgentConfigValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for SubAgentConfigValidationError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubAgentConfig {
    pub model: String,
    pub description: String,
    pub backend: Option<String>,
    pub system_prompt: Option<String>,
    pub max_cycles: u32,
    pub exclude_tools: Vec<String>,
    pub metadata: Metadata,
    pub denied_side_effects: Vec<ToolSideEffect>,
    pub denied_capability_tags: Vec<String>,
    pub deny_terminal_tools: bool,
    pub denied_cost_dimensions: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SubAgentConfigWire {
    model: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    backend: Option<String>,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default = "default_sub_agent_max_cycles")]
    max_cycles: u32,
    #[serde(default)]
    exclude_tools: Vec<String>,
    #[serde(default)]
    metadata: Metadata,
    #[serde(default)]
    denied_side_effects: Vec<ToolSideEffect>,
    #[serde(default)]
    denied_capability_tags: Vec<String>,
    #[serde(default)]
    deny_terminal_tools: bool,
    #[serde(default)]
    denied_cost_dimensions: Vec<String>,
}

const fn default_sub_agent_max_cycles() -> u32 {
    8
}

impl<'de> Deserialize<'de> for SubAgentConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        if !value.is_object() {
            return Err(D::Error::custom("SubAgentConfig payload must be an object"));
        }
        let wire = serde_json::from_value::<SubAgentConfigWire>(value).map_err(D::Error::custom)?;
        let mut config = Self {
            model: trim_portable_whitespace(&wire.model).to_string(),
            description: wire.description,
            backend: wire.backend,
            system_prompt: wire.system_prompt,
            max_cycles: wire.max_cycles,
            exclude_tools: wire.exclude_tools,
            metadata: wire.metadata,
            denied_side_effects: wire.denied_side_effects,
            denied_capability_tags: wire.denied_capability_tags,
            deny_terminal_tools: wire.deny_terminal_tools,
            denied_cost_dimensions: wire.denied_cost_dimensions,
        };
        config
            .normalize_policy_denials()
            .map_err(D::Error::custom)?;
        config.validate().map_err(D::Error::custom)?;
        Ok(config)
    }
}

impl SubAgentConfig {
    pub fn new(model: impl Into<String>, description: impl Into<String>) -> Self {
        let model = model.into();
        Self {
            model: trim_portable_whitespace(&model).to_string(),
            description: description.into(),
            backend: None,
            system_prompt: None,
            max_cycles: 8,
            exclude_tools: Vec::new(),
            metadata: Metadata::new(),
            denied_side_effects: Vec::new(),
            denied_capability_tags: Vec::new(),
            deny_terminal_tools: false,
            denied_cost_dimensions: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<(), SubAgentConfigValidationError> {
        if trim_portable_whitespace(&self.model).is_empty() {
            return Err(SubAgentConfigValidationError::new(
                INVALID_SUB_AGENT_MODEL_CODE,
                INVALID_SUB_AGENT_MODEL_MESSAGE,
            ));
        }
        if self
            .system_prompt
            .as_deref()
            .is_some_and(|prompt| trim_portable_whitespace(prompt).is_empty())
        {
            return Err(SubAgentConfigValidationError::new(
                INVALID_SUB_AGENT_SYSTEM_PROMPT_CODE,
                INVALID_SUB_AGENT_SYSTEM_PROMPT_MESSAGE,
            ));
        }
        self.declared_tool_policy().normalized().map_err(|_| {
            SubAgentConfigValidationError::new(
                "invalid_sub_agent_tool_policy",
                "sub-agent tool policy is invalid",
            )
        })?;
        Ok(())
    }

    pub fn declared_tool_policy(&self) -> ToolPolicy {
        ToolPolicy {
            denied_side_effects: self.denied_side_effects.clone(),
            denied_capability_tags: self.denied_capability_tags.clone(),
            deny_terminal_tools: self.deny_terminal_tools,
            denied_cost_dimensions: self.denied_cost_dimensions.clone(),
            ..ToolPolicy::default()
        }
    }

    fn normalize_policy_denials(&mut self) -> Result<(), crate::tools::ToolMetadataError> {
        let policy = self.declared_tool_policy().normalized()?;
        self.denied_side_effects = policy.denied_side_effects;
        self.denied_capability_tags = policy.denied_capability_tags;
        self.deny_terminal_tools = policy.deny_terminal_tools;
        self.denied_cost_dimensions = policy.denied_cost_dimensions;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
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
    pub sub_agents: BTreeMap<String, SubAgentConfig>,
    pub agent_type: Option<String>,
    pub native_multimodal: bool,
    pub extra_tool_names: Vec<String>,
    pub exclude_tools: Vec<String>,
    pub initial_messages: Vec<Message>,
    pub initial_shared_state: Metadata,
    pub model_settings: Option<ModelSettings>,
    pub metadata: Metadata,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentTaskWire {
    task_id: String,
    model: String,
    system_prompt: String,
    user_prompt: String,
    #[serde(default = "default_agent_task_max_cycles")]
    max_cycles: u32,
    #[serde(default = "default_memory_compact_threshold")]
    memory_compact_threshold: u64,
    #[serde(default = "default_memory_threshold_percentage")]
    memory_threshold_percentage: u8,
    #[serde(default)]
    no_tool_policy: NoToolPolicy,
    #[serde(default = "default_true")]
    allow_interruption: bool,
    #[serde(default = "default_true")]
    use_workspace: bool,
    #[serde(default)]
    sub_agents: BTreeMap<String, SubAgentConfig>,
    #[serde(default)]
    agent_type: Option<String>,
    #[serde(default)]
    native_multimodal: bool,
    #[serde(default)]
    extra_tool_names: Vec<String>,
    #[serde(default)]
    exclude_tools: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_agent_task_messages")]
    initial_messages: Vec<Message>,
    #[serde(default)]
    initial_shared_state: Metadata,
    #[serde(default, deserialize_with = "deserialize_agent_task_model_settings")]
    model_settings: Option<ModelSettings>,
    #[serde(default)]
    metadata: Metadata,
}

impl<'de> Deserialize<'de> for AgentTask {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        if !value.is_object() {
            return Err(D::Error::custom("AgentTask payload must be an object"));
        }
        let wire = serde_json::from_value::<AgentTaskWire>(value).map_err(D::Error::custom)?;
        Ok(Self {
            task_id: wire.task_id,
            model: wire.model,
            system_prompt: wire.system_prompt,
            user_prompt: wire.user_prompt,
            max_cycles: wire.max_cycles,
            memory_compact_threshold: wire.memory_compact_threshold,
            memory_threshold_percentage: wire.memory_threshold_percentage,
            no_tool_policy: wire.no_tool_policy,
            allow_interruption: wire.allow_interruption,
            use_workspace: wire.use_workspace,
            sub_agents: wire.sub_agents,
            agent_type: wire.agent_type,
            native_multimodal: wire.native_multimodal,
            extra_tool_names: wire.extra_tool_names,
            exclude_tools: wire.exclude_tools,
            initial_messages: wire.initial_messages,
            initial_shared_state: wire.initial_shared_state,
            model_settings: wire.model_settings,
            metadata: wire.metadata,
        })
    }
}

const fn default_agent_task_max_cycles() -> u32 {
    8
}

const fn default_memory_compact_threshold() -> u64 {
    250_000
}

const fn default_memory_threshold_percentage() -> u8 {
    90
}

const fn default_true() -> bool {
    true
}

fn deserialize_agent_task_messages<'de, D>(deserializer: D) -> Result<Vec<Message>, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<Value>::deserialize(deserializer)?
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            validate_agent_task_message(&value, index).map_err(D::Error::custom)?;
            Message::from_dict(&value).map_err(D::Error::custom)
        })
        .collect()
}

fn deserialize_agent_task_model_settings<'de, D>(
    deserializer: D,
) -> Result<Option<ModelSettings>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    value
        .map(|value| {
            if !value.is_object() {
                return Err(D::Error::custom(
                    "AgentTask field 'model_settings' must be an object or null",
                ));
            }
            serde_json::from_value(value).map_err(D::Error::custom)
        })
        .transpose()
}

fn validate_agent_task_message(value: &Value, index: usize) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("AgentTask initial_messages[{index}] must be an object"))?;
    let role = object
        .get("role")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("AgentTask initial_messages[{index}].role must be a string"))?;
    if !matches!(role, "system" | "user" | "assistant" | "tool") {
        return Err(format!(
            "unknown AgentTask initial_messages[{index}].role: {role}"
        ));
    }
    if object
        .get("content")
        .is_some_and(|value| !value.is_string())
    {
        return Err(format!(
            "AgentTask initial_messages[{index}].content must be a string"
        ));
    }
    for field_name in ["name", "tool_call_id", "reasoning_content", "image_url"] {
        if object
            .get(field_name)
            .is_some_and(|value| !value.is_null() && !value.is_string())
        {
            return Err(format!(
                "AgentTask initial_messages[{index}].{field_name} must be a string or null"
            ));
        }
    }
    if object.get("tool_calls").is_some_and(|value| {
        !value
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_object))
    }) {
        return Err(format!(
            "AgentTask initial_messages[{index}].tool_calls must be an array of objects"
        ));
    }
    if object
        .get("metadata")
        .is_some_and(|value| !value.is_object())
    {
        return Err(format!(
            "AgentTask initial_messages[{index}].metadata must be an object"
        ));
    }
    Ok(())
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
            memory_compact_threshold: default_memory_compact_threshold(),
            memory_threshold_percentage: 90,
            no_tool_policy: NoToolPolicy::Continue,
            allow_interruption: true,
            use_workspace: true,
            sub_agents: BTreeMap::new(),
            agent_type: None,
            native_multimodal: false,
            extra_tool_names: Vec::new(),
            exclude_tools: Vec::new(),
            initial_messages: Vec::new(),
            initial_shared_state: Metadata::new(),
            model_settings: None,
            metadata: Metadata::new(),
        }
    }

    pub fn sub_agents_enabled(&self) -> bool {
        !self.sub_agents.is_empty()
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_reason: Option<CompletionReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial_output: Option<String>,
    pub cycles: u32,
    pub todo_list: Vec<Value>,
    pub resolved: BTreeMap<String, String>,
}

impl Default for SubTaskOutcome {
    fn default() -> Self {
        Self {
            task_id: String::new(),
            agent_name: String::new(),
            status: AgentStatus::Pending,
            session_id: None,
            final_answer: None,
            wait_reason: None,
            error: None,
            error_code: None,
            completion_reason: None,
            completion_tool_name: None,
            partial_output: None,
            cycles: 0,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        }
    }
}

impl SubTaskOutcome {
    pub fn to_dict(&self) -> Value {
        self.to_value()
    }

    pub fn to_value(&self) -> Value {
        json_value_from_serializable(self)
    }
}
