use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod dict;

pub type Metadata = BTreeMap<String, Value>;
pub type ToolArguments = BTreeMap<String, Value>;
pub type ToolSchema = Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Pending,
    Running,
    WaitUser,
    Completed,
    Failed,
    MaxCycles,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolDirective {
    Continue,
    WaitUser,
    Finish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ToolResultStatus {
    Success,
    Error,
    WaitResponse,
    Running,
    PendingCompress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CycleStatus {
    Pending,
    Processing,
    Completed,
    WaitResponse,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: String,
    pub name: Option<String>,
    pub tool_call_id: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub reasoning_content: Option<String>,
    pub image_url: Option<String>,
    pub metadata: Metadata,
}

impl Message {
    pub fn new(role: MessageRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            name: None,
            tool_call_id: None,
            tool_calls: Vec::new(),
            reasoning_content: None,
            image_url: None,
            metadata: Metadata::new(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self::new(MessageRole::System, content)
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new(MessageRole::User, content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(MessageRole::Assistant, content)
    }

    pub fn tool(content: impl Into<String>, tool_call_id: impl Into<String>) -> Self {
        let mut message = Self::new(MessageRole::Tool, content);
        message.tool_call_id = Some(tool_call_id.into());
        message
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: ToolArguments,
    pub extra_content: Option<Value>,
}

impl ToolCall {
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: ToolArguments) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
            extra_content: None,
        }
    }

    pub fn from_raw_arguments(
        id: impl Into<String>,
        name: impl Into<String>,
        raw_arguments: Value,
    ) -> Self {
        let id = id.into();
        let name = name.into();
        match parse_raw_tool_arguments(&raw_arguments) {
            Ok(arguments) => Self {
                id,
                name,
                arguments,
                extra_content: None,
            },
            Err((error_code, error)) => Self {
                id,
                name,
                arguments: ToolArguments::new(),
                extra_content: Some(Value::Object(
                    [
                        ("raw_arguments".to_string(), raw_arguments),
                        ("argument_error_code".to_string(), Value::String(error_code)),
                        ("argument_error".to_string(), Value::String(error)),
                    ]
                    .into_iter()
                    .collect(),
                )),
            },
        }
    }
}

fn parse_raw_tool_arguments(raw_arguments: &Value) -> Result<ToolArguments, (String, String)> {
    match raw_arguments {
        Value::Null => Ok(ToolArguments::new()),
        Value::Object(object) => Ok(object.clone().into_iter().collect()),
        Value::String(raw) => {
            let stripped = raw.trim();
            if stripped.is_empty() {
                return Ok(ToolArguments::new());
            }
            let parsed = serde_json::from_str::<Value>(stripped).map_err(|error| {
                (
                    "invalid_arguments_json".to_string(),
                    format!("Invalid tool arguments JSON: {error}"),
                )
            })?;
            match parsed {
                Value::Object(object) => Ok(object.into_iter().collect()),
                _ => Err((
                    "invalid_arguments_payload".to_string(),
                    "Tool arguments must decode to an object".to_string(),
                )),
            }
        }
        other => Err((
            "invalid_arguments_type".to_string(),
            format!("Unsupported tool argument type: {}", json_type_name(other)),
        )),
    }
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cached_tokens: u64,
    pub reasoning_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub raw: Value,
}

impl TokenUsage {
    pub fn has_usage(&self) -> bool {
        self.prompt_tokens > 0
            || self.completion_tokens > 0
            || self.total_tokens > 0
            || self.cached_tokens > 0
            || self.reasoning_tokens > 0
            || self.input_tokens > 0
            || self.output_tokens > 0
            || self.cache_creation_tokens > 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CycleTokenUsage {
    pub cycle_index: u32,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskTokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cached_tokens: u64,
    pub reasoning_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cycles: Vec<CycleTokenUsage>,
}

impl TaskTokenUsage {
    pub fn add_cycle(&mut self, cycle_index: u32, usage: TokenUsage) {
        if !usage.has_usage() {
            return;
        }
        self.prompt_tokens += usage.prompt_tokens;
        self.completion_tokens += usage.completion_tokens;
        self.total_tokens += usage.total_tokens;
        self.cached_tokens += usage.cached_tokens;
        self.reasoning_tokens += usage.reasoning_tokens;
        self.input_tokens += usage.input_tokens;
        self.output_tokens += usage.output_tokens;
        self.cache_creation_tokens += usage.cache_creation_tokens;
        self.cycles.push(CycleTokenUsage { cycle_index, usage });
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolExecutionResult {
    pub tool_call_id: String,
    pub content: String,
    pub status: ToolResultStatus,
    pub directive: ToolDirective,
    pub error_code: Option<String>,
    pub metadata: Metadata,
    pub image_url: Option<String>,
    pub image_path: Option<String>,
}

impl ToolExecutionResult {
    pub fn success(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            status: ToolResultStatus::Success,
            directive: ToolDirective::Continue,
            error_code: None,
            metadata: Metadata::new(),
            image_url: None,
            image_path: None,
        }
    }

    pub fn error(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            status: ToolResultStatus::Error,
            ..Self::success(tool_call_id, content)
        }
    }

    pub fn to_message(&self) -> Message {
        Message::tool(self.content.clone(), self.tool_call_id.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LLMResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub raw: Metadata,
    pub token_usage: TokenUsage,
}

impl LLMResponse {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            tool_calls: Vec::new(),
            raw: Metadata::new(),
            token_usage: TokenUsage::default(),
        }
    }

    pub fn with_tool_calls(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            content: content.into(),
            tool_calls,
            raw: Metadata::new(),
            token_usage: TokenUsage::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CycleRecord {
    pub index: u32,
    pub assistant_message: String,
    pub tool_calls: Vec<ToolCall>,
    pub tool_results: Vec<ToolExecutionResult>,
    pub memory_compacted: bool,
    pub token_usage: TokenUsage,
}

impl CycleRecord {
    pub fn from_response(
        index: u32,
        response: &LLMResponse,
        tool_results: Vec<ToolExecutionResult>,
    ) -> Self {
        Self {
            index,
            assistant_message: response.content.clone(),
            tool_calls: response.tool_calls.clone(),
            tool_results,
            memory_compacted: false,
            token_usage: response.token_usage.clone(),
        }
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NoToolPolicy {
    #[default]
    Continue,
    WaitUser,
    Finish,
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

fn json_value_from_serializable<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentResult {
    pub status: AgentStatus,
    pub messages: Vec<Message>,
    pub cycles: Vec<CycleRecord>,
    pub final_answer: Option<String>,
    pub wait_reason: Option<String>,
    pub error: Option<String>,
    pub shared_state: Metadata,
    pub token_usage: TaskTokenUsage,
}

impl AgentResult {
    pub fn completed(
        messages: Vec<Message>,
        cycles: Vec<CycleRecord>,
        final_answer: impl Into<String>,
    ) -> Self {
        Self::completed_with_shared_state(messages, cycles, final_answer, Metadata::new())
    }

    pub fn completed_with_shared_state(
        messages: Vec<Message>,
        cycles: Vec<CycleRecord>,
        final_answer: impl Into<String>,
        shared_state: Metadata,
    ) -> Self {
        let mut token_usage = TaskTokenUsage::default();
        for cycle in &cycles {
            token_usage.add_cycle(cycle.index, cycle.token_usage.clone());
        }
        Self {
            status: AgentStatus::Completed,
            messages,
            cycles,
            final_answer: Some(final_answer.into()),
            wait_reason: None,
            error: None,
            shared_state,
            token_usage,
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            status: AgentStatus::Failed,
            messages: Vec::new(),
            cycles: Vec::new(),
            final_answer: None,
            wait_reason: None,
            error: Some(error.into()),
            shared_state: Metadata::new(),
            token_usage: TaskTokenUsage::default(),
        }
    }

    pub fn todo_list(&self) -> Vec<Value> {
        self.shared_state
            .get("todo_list")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    }
}
