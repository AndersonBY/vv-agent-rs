use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::config::ResolvedModelConfig;
use crate::llm::{LlmClient, ScriptedLlmClient};
use crate::runtime::AgentRuntime;
use crate::types::{AgentResult, AgentStatus, AgentTask, Metadata, NoToolPolicy, SubAgentConfig};

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

#[derive(Debug, Clone)]
pub struct AgentSDKOptions {
    pub settings_file: PathBuf,
    pub default_backend: String,
    pub workspace: PathBuf,
    pub timeout_seconds: f64,
    pub log_preview_chars: Option<usize>,
    pub auto_discover_resources: bool,
    pub debug_dump_dir: Option<String>,
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
            Value::String(format!("{:?}", self.result.status).to_lowercase()),
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
        payload
    }
}

#[derive(Debug, Clone)]
pub struct AgentSessionState {
    pub running: bool,
    pub workspace: PathBuf,
    pub messages: Vec<crate::types::Message>,
    pub shared_state: Metadata,
    pub latest_run: Option<AgentRun>,
}

pub type SessionEventHandler = Arc<dyn Fn(&str, &BTreeMap<String, Value>) + Send + Sync + 'static>;

pub struct AgentSession {
    execute_run: Arc<dyn Fn(String) -> Result<AgentRun, String> + Send + Sync>,
    _session_id: String,
    _agent_name: String,
    _definition: AgentDefinition,
    workspace: PathBuf,
    shared_state: Metadata,
    messages: Vec<crate::types::Message>,
    latest_run: Option<AgentRun>,
    running: bool,
    _listeners: Vec<SessionEventHandler>,
}

impl AgentSession {
    pub fn new(
        execute_run: Arc<dyn Fn(String) -> Result<AgentRun, String> + Send + Sync>,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        workspace: impl Into<PathBuf>,
    ) -> Self {
        Self {
            execute_run,
            _session_id: format!(
                "session-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_nanos())
                    .unwrap_or_default()
            ),
            _agent_name: agent_name.into(),
            _definition: definition,
            workspace: workspace.into(),
            shared_state: Metadata::new(),
            messages: Vec::new(),
            latest_run: None,
            running: false,
            _listeners: Vec::new(),
        }
    }

    pub fn prompt(&mut self, prompt: impl Into<String>) -> Result<AgentRun, String> {
        let run = (self.execute_run)(prompt.into())?;
        self.messages = run.result.messages.clone();
        self.shared_state = run.result.shared_state.clone();
        self.latest_run = Some(run.clone());
        Ok(run)
    }

    pub fn state(&self) -> AgentSessionState {
        AgentSessionState {
            running: self.running,
            workspace: self.workspace.clone(),
            messages: self.messages.clone(),
            shared_state: self.shared_state.clone(),
            latest_run: self.latest_run.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiscoveredResources {
    pub agents: BTreeMap<String, AgentDefinition>,
    pub prompts: BTreeMap<String, String>,
    pub skill_directories: Vec<String>,
    pub diagnostics: Vec<String>,
}

pub struct AgentResourceLoader {
    pub workspace: PathBuf,
    pub project_resource_dir: PathBuf,
    pub global_resource_dir: PathBuf,
    cached: Option<DiscoveredResources>,
}

impl AgentResourceLoader {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        let workspace = workspace.into();
        Self {
            project_resource_dir: workspace.join(".vv-agent"),
            global_resource_dir: PathBuf::from("~/.vv-agent"),
            workspace,
            cached: None,
        }
    }

    pub fn discover(&mut self) -> DiscoveredResources {
        if let Some(cached) = &self.cached {
            return cached.clone();
        }
        let discovered = DiscoveredResources::default();
        self.cached = Some(discovered.clone());
        discovered
    }
}

#[derive(Clone)]
pub struct AgentSDKClient {
    pub options: AgentSDKOptions,
    default_agent: Option<AgentDefinition>,
    runtime: Arc<dyn RunAgent + Send + Sync>,
}

pub trait RunAgent {
    fn run(&self, definition: &AgentDefinition, prompt: String) -> Result<AgentRun, String>;
}

impl<C: LlmClient + 'static> RunAgent for AgentRuntime<C> {
    fn run(&self, definition: &AgentDefinition, prompt: String) -> Result<AgentRun, String> {
        let task = task_from_definition(definition, prompt);
        let result = AgentRuntime::run(self, task).map_err(|err| err.to_string())?;
        let resolved = ResolvedModelConfig::new(
            definition
                .backend
                .clone()
                .unwrap_or_else(|| "moonshot".to_string()),
            definition.model.clone(),
            definition.model.clone(),
            definition.model.clone(),
            Vec::new(),
        );
        Ok(AgentRun {
            agent_name: definition.model.clone(),
            result,
            resolved,
        })
    }
}

impl RunAgent for ScriptedLlmClient {
    fn run(&self, definition: &AgentDefinition, prompt: String) -> Result<AgentRun, String> {
        let runtime = AgentRuntime::new(self.clone());
        let task = task_from_definition(definition, prompt);
        runtime
            .run(task)
            .map_err(|err| err.to_string())
            .map(|result| AgentRun {
                agent_name: definition.model.clone(),
                resolved: ResolvedModelConfig::new(
                    definition
                        .backend
                        .clone()
                        .unwrap_or_else(|| "moonshot".to_string()),
                    definition.model.clone(),
                    definition.model.clone(),
                    definition.model.clone(),
                    Vec::new(),
                ),
                result,
            })
    }
}

fn task_from_definition(definition: &AgentDefinition, prompt: String) -> AgentTask {
    let mut task = AgentTask::new(
        format!("{}-task", definition.model),
        definition.model.clone(),
        definition.system_prompt.clone().unwrap_or_default(),
        prompt,
    );
    task.max_cycles = definition.max_cycles;
    task.memory_compact_threshold = definition.memory_compact_threshold;
    task.memory_threshold_percentage = definition.memory_threshold_percentage;
    task.no_tool_policy = definition.no_tool_policy;
    task.allow_interruption = definition.allow_interruption;
    task.use_workspace = definition.use_workspace;
    task.has_sub_agents = definition.enable_sub_agents;
    task.sub_agents = definition.sub_agents.clone();
    task.agent_type = definition.agent_type.clone();
    task.native_multimodal = definition.native_multimodal;
    task.extra_tool_names = definition.extra_tool_names.clone();
    task.exclude_tools = definition.exclude_tools.clone();
    task.metadata = definition.metadata.clone();
    task
}

impl AgentSDKClient {
    pub fn new(options: AgentSDKOptions) -> Self {
        Self {
            options,
            default_agent: None,
            runtime: Arc::new(NullRunAgent),
        }
    }

    pub fn with_runtime<C: LlmClient + 'static>(mut self, runtime: AgentRuntime<C>) -> Self {
        self.runtime = Arc::new(runtime);
        self
    }

    pub fn set_default_agent(&mut self, definition: AgentDefinition) {
        self.default_agent = Some(definition);
    }

    pub fn run_with_agent(
        &self,
        definition: AgentDefinition,
        prompt: impl Into<String>,
    ) -> Result<AgentRun, String> {
        self.runtime.run(&definition, prompt.into())
    }

    pub fn run(&self, prompt: impl Into<String>) -> Result<AgentRun, String> {
        let agent = self
            .default_agent
            .clone()
            .unwrap_or_else(|| AgentDefinition::default_for_model("demo"));
        self.run_with_agent(agent, prompt)
    }
}

struct NullRunAgent;

impl RunAgent for NullRunAgent {
    fn run(&self, _definition: &AgentDefinition, _prompt: String) -> Result<AgentRun, String> {
        Err("runtime not configured".to_string())
    }
}

pub fn create_agent_session(
    client: &AgentSDKClient,
    agent_name: impl Into<String>,
    definition: AgentDefinition,
) -> AgentSession {
    let runtime = client.runtime.clone();
    let definition_for_run = definition.clone();
    let execute_run = Arc::new(move |prompt: String| runtime.run(&definition_for_run, prompt));
    AgentSession::new(execute_run, agent_name, definition, "./workspace")
}

pub fn run(client: &AgentSDKClient, prompt: impl Into<String>) -> Result<AgentRun, String> {
    client.run(prompt)
}

pub fn query(client: &AgentSDKClient, prompt: impl Into<String>) -> Result<String, String> {
    let run = client.run(prompt)?;
    if run.result.status == AgentStatus::Completed {
        Ok(run.result.final_answer.unwrap_or_default())
    } else {
        Err(format!("agent did not complete: {:?}", run.result.status))
    }
}
