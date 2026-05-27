use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

use crate::config::ResolvedModelConfig;
use crate::runtime::StreamCallback;
use crate::types::{AgentResult, AgentStatus, Metadata, NoToolPolicy, SubAgentConfig};

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
            description: "Rust vv-agent profile".to_string(),
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

#[derive(Clone)]
pub struct AgentSDKOptions {
    pub settings_file: PathBuf,
    pub default_backend: String,
    pub workspace: PathBuf,
    pub timeout_seconds: f64,
    pub log_preview_chars: Option<usize>,
    pub auto_discover_resources: bool,
    pub debug_dump_dir: Option<String>,
    pub stream_callback: Option<StreamCallback>,
}

impl std::fmt::Debug for AgentSDKOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentSDKOptions")
            .field("settings_file", &self.settings_file)
            .field("default_backend", &self.default_backend)
            .field("workspace", &self.workspace)
            .field("timeout_seconds", &self.timeout_seconds)
            .field("log_preview_chars", &self.log_preview_chars)
            .field("auto_discover_resources", &self.auto_discover_resources)
            .field("debug_dump_dir", &self.debug_dump_dir)
            .field("has_stream_callback", &self.stream_callback.is_some())
            .finish()
    }
}

impl Default for AgentSDKOptions {
    fn default() -> Self {
        Self {
            settings_file: PathBuf::from("local_settings.py"),
            default_backend: "moonshot".to_string(),
            workspace: PathBuf::from("./workspace"),
            timeout_seconds: 90.0,
            log_preview_chars: None,
            auto_discover_resources: true,
            debug_dump_dir: None,
            stream_callback: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentRun {
    pub agent_name: String,
    pub result: AgentResult,
    pub resolved: ResolvedModelConfig,
}

impl AgentRun {
    pub fn to_dict(&self) -> BTreeMap<String, Value> {
        let mut payload = BTreeMap::new();
        payload.insert("agent".to_string(), Value::String(self.agent_name.clone()));
        payload.insert(
            "status".to_string(),
            Value::String(agent_status_value(self.result.status).to_string()),
        );
        payload.insert(
            "final_answer".to_string(),
            self.result
                .final_answer
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        payload.insert(
            "cycles".to_string(),
            Value::Number(serde_json::Number::from(self.result.cycles.len() as u64)),
        );
        payload.insert(
            "token_usage".to_string(),
            serde_json::to_value(&self.result.token_usage).unwrap_or(Value::Null),
        );
        payload
    }
}

pub(crate) fn agent_status_value(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Pending => "pending",
        AgentStatus::Running => "running",
        AgentStatus::WaitUser => "wait_user",
        AgentStatus::Completed => "completed",
        AgentStatus::Failed => "failed",
        AgentStatus::MaxCycles => "max_cycles",
    }
}

pub(crate) fn query_text_from_run(
    run: AgentRun,
    require_completed: bool,
    error_prefix: &str,
) -> Result<String, String> {
    if run.result.status == AgentStatus::Completed {
        return Ok(run.result.final_answer.unwrap_or_default());
    }
    if require_completed {
        let reason = run
            .result
            .error
            .clone()
            .or(run.result.wait_reason.clone())
            .or(run.result.final_answer.clone())
            .unwrap_or_else(|| "query did not complete successfully".to_string());
        return Err(format!(
            "{error_prefix} with status={}: {}",
            agent_status_value(run.result.status),
            reason
        ));
    }
    Ok(run
        .result
        .final_answer
        .or(run.result.wait_reason)
        .or(run.result.error)
        .unwrap_or_default())
}
