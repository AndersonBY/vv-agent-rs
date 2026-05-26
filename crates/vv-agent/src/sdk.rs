use std::collections::{BTreeMap, VecDeque};
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
    steering_queue: VecDeque<String>,
    follow_up_queue: VecDeque<String>,
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
            steering_queue: VecDeque::new(),
            follow_up_queue: VecDeque::new(),
        }
    }

    pub fn prompt(&mut self, prompt: impl Into<String>) -> Result<AgentRun, String> {
        self.prompt_with_auto_follow_up(prompt, true)
    }

    pub fn prompt_with_auto_follow_up(
        &mut self,
        prompt: impl Into<String>,
        auto_follow_up: bool,
    ) -> Result<AgentRun, String> {
        let mut run = self.run_once(normalize_session_prompt(prompt.into(), "prompt")?)?;
        if !auto_follow_up {
            return Ok(run);
        }

        while run.result.status == AgentStatus::Completed {
            let Some(follow_up_prompt) = self.follow_up_queue.pop_front() else {
                break;
            };
            run = self.run_once(follow_up_prompt)?;
        }
        Ok(run)
    }

    pub fn follow_up(&mut self, prompt: impl Into<String>) -> Result<(), String> {
        self.follow_up_queue
            .push_back(normalize_session_prompt(prompt.into(), "follow_up prompt")?);
        Ok(())
    }

    pub fn steer(&mut self, prompt: impl Into<String>) -> Result<(), String> {
        self.steering_queue
            .push_back(normalize_session_prompt(prompt.into(), "steer prompt")?);
        Ok(())
    }

    pub fn clear_queues(&mut self) {
        self.steering_queue.clear();
        self.follow_up_queue.clear();
    }

    pub fn continue_run(&mut self, prompt: Option<String>) -> Result<AgentRun, String> {
        if let Some(prompt) = prompt {
            let prompt = prompt.trim();
            if !prompt.is_empty() {
                return self.prompt_with_auto_follow_up(prompt.to_string(), false);
            }
        }

        let queued_prompt = self
            .steering_queue
            .pop_front()
            .or_else(|| self.follow_up_queue.pop_front())
            .ok_or_else(|| {
                "No queued prompt available. Provide prompt or call steer()/follow_up() first."
                    .to_string()
            })?;
        self.prompt_with_auto_follow_up(queued_prompt, false)
    }

    pub fn query(&mut self, prompt: impl Into<String>) -> Result<String, String> {
        self.query_with_require_completed(prompt, true)
    }

    pub fn query_with_require_completed(
        &mut self,
        prompt: impl Into<String>,
        require_completed: bool,
    ) -> Result<String, String> {
        let run = self.prompt(prompt)?;
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
                .unwrap_or_else(|| "session query did not complete".to_string());
            return Err(format!(
                "Session query failed with status={}: {}",
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

    fn run_once(&mut self, prompt: String) -> Result<AgentRun, String> {
        if self.running {
            return Err(
                "Session is already running. Queue with steer()/follow_up() or wait for completion."
                    .to_string(),
            );
        }
        self.running = true;
        let run = (self.execute_run)(prompt);
        self.running = false;
        let run = run?;
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

fn normalize_session_prompt(prompt: String, label: &str) -> Result<String, String> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Err(format!("{label} cannot be empty"));
    }
    Ok(prompt.to_string())
}

fn agent_status_value(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Pending => "pending",
        AgentStatus::Running => "running",
        AgentStatus::WaitUser => "wait_user",
        AgentStatus::Completed => "completed",
        AgentStatus::Failed => "failed",
        AgentStatus::MaxCycles => "max_cycles",
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

    pub fn with_resource_dirs(
        workspace: impl Into<PathBuf>,
        project_resource_dir: impl Into<PathBuf>,
        global_resource_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            workspace: workspace.into(),
            project_resource_dir: project_resource_dir.into(),
            global_resource_dir: global_resource_dir.into(),
            cached: None,
        }
    }

    pub fn discover(&mut self) -> DiscoveredResources {
        if let Some(cached) = &self.cached {
            return cached.clone();
        }
        let mut discovered = DiscoveredResources::default();
        for root in [
            self.global_resource_dir.clone(),
            self.project_resource_dir.clone(),
        ] {
            if root.is_dir() {
                self.load_agents(&root, &mut discovered);
                self.load_prompts(&root, &mut discovered);
                self.load_skills(&root, &mut discovered);
            }
        }
        self.cached = Some(discovered.clone());
        discovered
    }

    fn load_agents(&self, root: &std::path::Path, discovered: &mut DiscoveredResources) {
        let config_file = root.join("agents.json");
        if !config_file.is_file() {
            return;
        }
        let raw = match std::fs::read_to_string(&config_file)
            .ok()
            .and_then(|content| serde_json::from_str::<Value>(&content).ok())
        {
            Some(raw) => raw,
            None => {
                discovered
                    .diagnostics
                    .push(format!("Invalid agents.json in {}", root.display()));
                return;
            }
        };
        let profiles = raw
            .get("profiles")
            .and_then(Value::as_object)
            .or_else(|| raw.as_object());
        let Some(profiles) = profiles else {
            discovered.diagnostics.push(format!(
                "agents.json in {} must be an object or contain `profiles` object.",
                root.display()
            ));
            return;
        };
        for (name, payload) in profiles {
            let Some(payload) = payload.as_object() else {
                continue;
            };
            let Some(description) = payload.get("description").and_then(Value::as_str) else {
                continue;
            };
            let Some(model) = payload.get("model").and_then(Value::as_str) else {
                continue;
            };
            let mut definition = AgentDefinition::default_for_model(model);
            definition.description = description.to_string();
            definition.backend = read_string(payload, "backend");
            definition.language =
                read_string(payload, "language").unwrap_or_else(|| "zh-CN".to_string());
            definition.max_cycles = read_u32(payload, "max_cycles", 10).max(1);
            definition.memory_compact_threshold =
                read_u64(payload, "memory_compact_threshold", 128_000);
            definition.memory_threshold_percentage =
                read_u8(payload, "memory_threshold_percentage", 90);
            definition.no_tool_policy = read_no_tool_policy(payload);
            definition.allow_interruption = read_bool(payload, "allow_interruption", true);
            definition.use_workspace = read_bool(payload, "use_workspace", true);
            definition.enable_todo_management = read_bool(payload, "enable_todo_management", true);
            definition.agent_type = read_string(payload, "agent_type");
            definition.native_multimodal = read_bool(payload, "native_multimodal", false);
            definition.enable_sub_agents = read_bool(payload, "enable_sub_agents", false);
            definition.sub_agents = read_sub_agents(payload);
            definition.extra_tool_names = read_string_list(payload, "extra_tool_names");
            definition.exclude_tools = read_string_list(payload, "exclude_tools");
            definition.bash_shell = read_string(payload, "bash_shell");
            definition.windows_shell_priority = read_string_list(payload, "windows_shell_priority");
            definition.bash_env = read_string_map(payload, "bash_env");
            definition.metadata = read_metadata(payload, "metadata");
            definition.system_prompt = read_string(payload, "system_prompt");
            definition.system_prompt_template = read_string(payload, "system_prompt_template");
            definition.skill_directories =
                read_resolved_path_list(payload, "skill_directories", root);
            discovered.agents.insert(name.clone(), definition);
        }
    }

    fn load_prompts(&self, root: &std::path::Path, discovered: &mut DiscoveredResources) {
        let prompts_dir = root.join("prompts");
        if !prompts_dir.is_dir() {
            return;
        }
        let Ok(entries) = std::fs::read_dir(prompts_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                continue;
            }
            if let (Some(stem), Ok(content)) = (
                path.file_stem().and_then(|stem| stem.to_str()),
                std::fs::read_to_string(&path),
            ) {
                discovered.prompts.insert(stem.to_string(), content);
            }
        }
    }

    fn load_skills(&self, root: &std::path::Path, discovered: &mut DiscoveredResources) {
        let skills_dir = root.join("skills");
        if !skills_dir.is_dir() {
            return;
        }
        let path = skills_dir.to_string_lossy().to_string();
        if !discovered.skill_directories.contains(&path) {
            discovered.skill_directories.push(path);
        }
    }
}

fn read_string(payload: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn read_bool(payload: &serde_json::Map<String, Value>, key: &str, default: bool) -> bool {
    payload.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn read_u32(payload: &serde_json::Map<String, Value>, key: &str, default: u32) -> u32 {
    payload
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(default)
}

fn read_u64(payload: &serde_json::Map<String, Value>, key: &str, default: u64) -> u64 {
    payload.get(key).and_then(Value::as_u64).unwrap_or(default)
}

fn read_u8(payload: &serde_json::Map<String, Value>, key: &str, default: u8) -> u8 {
    payload
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u8::try_from(value).ok())
        .unwrap_or(default)
}

fn read_no_tool_policy(payload: &serde_json::Map<String, Value>) -> NoToolPolicy {
    match read_string(payload, "no_tool_policy").as_deref() {
        Some("finish") => NoToolPolicy::Finish,
        Some("wait_user") => NoToolPolicy::WaitUser,
        _ => NoToolPolicy::Continue,
    }
}

fn read_string_list(payload: &serde_json::Map<String, Value>, key: &str) -> Vec<String> {
    payload
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn read_string_map(
    payload: &serde_json::Map<String, Value>,
    key: &str,
) -> BTreeMap<String, String> {
    payload
        .get(key)
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .filter_map(|(key, value)| {
                    let key = key.trim();
                    if key.is_empty() {
                        return None;
                    }
                    Some((
                        key.to_string(),
                        value
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| value.to_string()),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn read_metadata(payload: &serde_json::Map<String, Value>, key: &str) -> Metadata {
    payload
        .get(key)
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn read_resolved_path_list(
    payload: &serde_json::Map<String, Value>,
    key: &str,
    base_dir: &std::path::Path,
) -> Vec<String> {
    payload
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|path| resolve_resource_path(base_dir, path))
                .collect()
        })
        .unwrap_or_default()
}

fn resolve_resource_path(base_dir: &std::path::Path, raw_path: &str) -> String {
    let path = PathBuf::from(raw_path);
    let path = if path.is_absolute() {
        path
    } else {
        base_dir.join(path)
    };
    path.to_string_lossy().to_string()
}

fn read_sub_agents(payload: &serde_json::Map<String, Value>) -> BTreeMap<String, SubAgentConfig> {
    let mut sub_agents = BTreeMap::new();
    let Some(object) = payload.get("sub_agents").and_then(Value::as_object) else {
        return sub_agents;
    };
    for (name, raw_config) in object {
        let Some(config) = raw_config.as_object() else {
            continue;
        };
        let Some(model) = read_string(config, "model") else {
            continue;
        };
        let Some(description) = read_string(config, "description") else {
            continue;
        };
        let mut sub_agent = SubAgentConfig::new(model, description);
        sub_agent.backend = read_string(config, "backend");
        sub_agent.system_prompt = read_string(config, "system_prompt");
        sub_agent.max_cycles = read_u32(config, "max_cycles", 8).max(1);
        sub_agent.exclude_tools = read_string_list(config, "exclude_tools");
        sub_agent.metadata = read_metadata(config, "metadata");
        sub_agents.insert(name.clone(), sub_agent);
    }
    sub_agents
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

impl<C: LlmClient + Clone + 'static> RunAgent for AgentRuntime<C> {
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

    pub fn with_runtime<C: LlmClient + Clone + 'static>(
        mut self,
        runtime: AgentRuntime<C>,
    ) -> Self {
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
