use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use crate::prompt::{
    build_raw_system_prompt_sections, build_system_prompt_bundle_with_options,
    BuildSystemPromptOptions,
};
use crate::types::{AgentTask, Metadata};

use super::super::session::AgentSessionRunRequest;
use super::super::types::AgentDefinition;
use super::AgentSDKClient;

static SDK_TASK_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(super) fn task_from_definition_with_task_name(
    definition: &AgentDefinition,
    prompt: String,
    workspace: Option<&Path>,
    task_name: Option<&str>,
) -> AgentTask {
    let (system_prompt, system_prompt_sections) =
        system_prompt_from_definition(definition, workspace);
    let mut metadata = definition.metadata.clone();
    metadata
        .entry("language".to_string())
        .or_insert_with(|| Value::String(definition.language.clone()));
    if let Some(shell) = definition.bash_shell.as_ref() {
        metadata
            .entry("bash_shell".to_string())
            .or_insert_with(|| Value::String(shell.clone()));
    }
    if !definition.windows_shell_priority.is_empty() {
        metadata
            .entry("windows_shell_priority".to_string())
            .or_insert_with(|| {
                Value::Array(
                    definition
                        .windows_shell_priority
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                )
            });
    }
    if !definition.bash_env.is_empty() {
        metadata
            .entry("bash_env".to_string())
            .or_insert_with(|| serde_json::to_value(&definition.bash_env).unwrap_or(Value::Null));
    }
    if !definition.sub_agents.is_empty() {
        metadata
            .entry("sub_agent_names".to_string())
            .or_insert_with(|| {
                Value::Array(
                    definition
                        .sub_agents
                        .keys()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                )
            });
    }
    if !definition.skill_directories.is_empty() {
        metadata
            .entry("available_skills".to_string())
            .or_insert_with(|| {
                Value::Array(
                    definition
                        .skill_directories
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                )
            });
    }
    let mut task = AgentTask::new(
        generate_task_id(task_name.unwrap_or("inline")),
        definition.model.clone(),
        system_prompt,
        prompt,
    );
    task.max_cycles = definition.max_cycles.max(1);
    task.memory_compact_threshold = definition.memory_compact_threshold.max(1);
    task.memory_threshold_percentage = definition.memory_threshold_percentage.clamp(1, 100);
    task.no_tool_policy = definition.no_tool_policy;
    task.allow_interruption = definition.allow_interruption;
    task.use_workspace = definition.use_workspace;
    task.has_sub_agents = definition.enable_sub_agents;
    task.sub_agents = definition.sub_agents.clone();
    task.agent_type = definition.agent_type.clone();
    task.native_multimodal = definition.native_multimodal;
    task.extra_tool_names = definition.extra_tool_names.clone();
    task.exclude_tools = definition.exclude_tools.clone();
    task.metadata = metadata;
    if !system_prompt_sections.is_empty() {
        task.metadata
            .entry("system_prompt_sections".to_string())
            .or_insert(Value::Array(system_prompt_sections));
    }
    task
}

fn generate_task_id(prefix: &str) -> String {
    let normalized_prefix = prefix.trim();
    let prefix = if normalized_prefix.is_empty() {
        "inline"
    } else {
        normalized_prefix
    };
    let counter = SDK_TASK_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
    format!("{prefix}_{:08x}", counter & 0xffff_ffff)
}

pub(super) fn normalize_prepare_session_id(session_id: Option<String>) -> Option<String> {
    session_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn system_prompt_from_definition(
    definition: &AgentDefinition,
    workspace: Option<&Path>,
) -> (String, Vec<Value>) {
    if let Some(system_prompt) = definition.system_prompt.as_ref() {
        return (
            system_prompt.clone(),
            build_raw_system_prompt_sections(system_prompt),
        );
    }

    let available_sub_agents = definition
        .sub_agents
        .iter()
        .map(|(name, config)| (name.clone(), config.description.clone()))
        .collect();
    let available_skills = definition
        .metadata
        .get("available_skills")
        .cloned()
        .or_else(|| {
            (!definition.skill_directories.is_empty()).then(|| {
                Value::Array(
                    definition
                        .skill_directories
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                )
            })
        });
    let prompt_bundle = build_system_prompt_bundle_with_options(
        &definition.description,
        BuildSystemPromptOptions {
            language: definition.language.clone(),
            allow_interruption: definition.allow_interruption,
            use_workspace: definition.use_workspace,
            enable_todo_management: definition.enable_todo_management,
            agent_type: definition.agent_type.clone(),
            available_sub_agents,
            available_skills,
            workspace: workspace.map(Path::to_path_buf),
            ..BuildSystemPromptOptions::default()
        },
    );
    (prompt_bundle.prompt, prompt_bundle.sections)
}

pub(super) fn merge_request_metadata(task: &mut AgentTask, metadata: Metadata) {
    for (key, value) in metadata {
        task.metadata.entry(key).or_insert(value);
    }
}

impl AgentSDKClient {
    pub fn prepare_task_for_agent(
        &self,
        agent_name: impl AsRef<str>,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
    ) -> Result<AgentTask, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(self.prepare_task_with_named_agent_in_workspace(
            agent_name,
            definition,
            prompt,
            resolved_model_id,
            self.options.workspace.clone(),
            None::<String>,
        ))
    }

    pub fn prepare_task_for_agent_with_request(
        &self,
        agent_name: impl AsRef<str>,
        request: AgentSessionRunRequest,
        resolved_model_id: impl Into<String>,
    ) -> Result<AgentTask, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(self.prepare_task_with_named_agent_request(
            agent_name,
            definition,
            request,
            resolved_model_id,
        ))
    }

    pub fn prepare_task_for_agent_with_session_id(
        &self,
        agent_name: impl AsRef<str>,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Result<AgentTask, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(self.prepare_task_with_named_agent_in_workspace(
            agent_name,
            definition,
            prompt,
            resolved_model_id,
            self.options.workspace.clone(),
            Some(session_id.into()),
        ))
    }

    pub fn prepare_task_for_agent_in_workspace(
        &self,
        agent_name: impl AsRef<str>,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
    ) -> Result<AgentTask, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(self.prepare_task_with_named_agent_in_workspace(
            agent_name,
            definition,
            prompt,
            resolved_model_id,
            workspace,
            None::<String>,
        ))
    }

    pub fn prepare_task_for_agent_in_workspace_with_session_id(
        &self,
        agent_name: impl AsRef<str>,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
        session_id: impl Into<String>,
    ) -> Result<AgentTask, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(self.prepare_task_with_named_agent_in_workspace(
            agent_name,
            definition,
            prompt,
            resolved_model_id,
            workspace,
            Some(session_id.into()),
        ))
    }

    pub fn prepare_task_with_agent(
        &self,
        definition: AgentDefinition,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
    ) -> AgentTask {
        self.prepare_task_with_named_agent_in_workspace(
            "inline",
            definition,
            prompt,
            resolved_model_id,
            self.options.workspace.clone(),
            None::<String>,
        )
    }

    pub fn prepare_task_with_agent_request(
        &self,
        definition: AgentDefinition,
        request: AgentSessionRunRequest,
        resolved_model_id: impl Into<String>,
    ) -> AgentTask {
        self.prepare_task_with_named_agent_request("inline", definition, request, resolved_model_id)
    }

    pub fn prepare_task_with_agent_with_session_id(
        &self,
        definition: AgentDefinition,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> AgentTask {
        self.prepare_task_with_agent_in_workspace_with_session_id(
            definition,
            prompt,
            resolved_model_id,
            self.options.workspace.clone(),
            session_id,
        )
    }

    pub fn prepare_task_with_agent_in_workspace(
        &self,
        definition: AgentDefinition,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
    ) -> AgentTask {
        self.prepare_task_with_named_agent_in_workspace(
            "inline",
            definition,
            prompt,
            resolved_model_id,
            workspace,
            None::<String>,
        )
    }

    pub fn prepare_task_with_agent_in_workspace_with_session_id(
        &self,
        definition: AgentDefinition,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
        session_id: impl Into<String>,
    ) -> AgentTask {
        self.prepare_task_with_named_agent_in_workspace(
            "inline",
            definition,
            prompt,
            resolved_model_id,
            workspace,
            Some(session_id.into()),
        )
    }

    pub fn prepare_task(
        &self,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
    ) -> Result<AgentTask, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call prepare_task_with_agent(...) or register named agents first.",
            "Multiple agents configured. Call prepare_task_for_agent(name, ...) with one of:",
        )?;
        Ok(self.prepare_task_with_named_agent_in_workspace(
            &name,
            definition,
            prompt,
            resolved_model_id,
            self.options.workspace.clone(),
            None::<String>,
        ))
    }

    pub fn prepare_task_with_request(
        &self,
        request: AgentSessionRunRequest,
        resolved_model_id: impl Into<String>,
    ) -> Result<AgentTask, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call prepare_task_with_agent_request(...) or register named agents first.",
            "Multiple agents configured. Call prepare_task_for_agent_with_request(name, ...) with one of:",
        )?;
        Ok(self.prepare_task_with_named_agent_request(
            &name,
            definition,
            request,
            resolved_model_id,
        ))
    }

    pub fn prepare_task_with_session_id(
        &self,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Result<AgentTask, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call prepare_task_with_agent_with_session_id(...) or register named agents first.",
            "Multiple agents configured. Call prepare_task_for_agent_with_session_id(name, ...) with one of:",
        )?;
        Ok(self.prepare_task_with_named_agent_in_workspace(
            &name,
            definition,
            prompt,
            resolved_model_id,
            self.options.workspace.clone(),
            Some(session_id.into()),
        ))
    }

    pub fn prepare_task_in_workspace(
        &self,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
    ) -> Result<AgentTask, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call prepare_task_with_agent_in_workspace(...) or register named agents first.",
            "Multiple agents configured. Call prepare_task_for_agent_in_workspace(name, ...) with one of:",
        )?;
        Ok(self.prepare_task_with_named_agent_in_workspace(
            &name,
            definition,
            prompt,
            resolved_model_id,
            workspace,
            None::<String>,
        ))
    }

    pub fn prepare_task_in_workspace_with_session_id(
        &self,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
        session_id: impl Into<String>,
    ) -> Result<AgentTask, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call prepare_task_with_agent_in_workspace_with_session_id(...) or register named agents first.",
            "Multiple agents configured. Call prepare_task_for_agent_in_workspace_with_session_id(name, ...) with one of:",
        )?;
        Ok(self.prepare_task_with_named_agent_in_workspace(
            &name,
            definition,
            prompt,
            resolved_model_id,
            workspace,
            Some(session_id.into()),
        ))
    }

    fn prepare_task_with_named_agent_in_workspace(
        &self,
        agent_name: &str,
        definition: AgentDefinition,
        prompt: impl Into<String>,
        resolved_model_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
        session_id: Option<String>,
    ) -> AgentTask {
        let mut request = AgentSessionRunRequest::new(prompt);
        request.workspace = Some(workspace.into());
        if let Some(session_id) = normalize_prepare_session_id(session_id) {
            request
                .metadata
                .entry("session_id".to_string())
                .or_insert(Value::String(session_id));
        }
        self.prepare_task_with_named_agent_request(
            agent_name,
            definition,
            request,
            resolved_model_id,
        )
    }

    fn prepare_task_with_named_agent_request(
        &self,
        agent_name: &str,
        definition: AgentDefinition,
        mut request: AgentSessionRunRequest,
        resolved_model_id: impl Into<String>,
    ) -> AgentTask {
        let workspace = request
            .workspace
            .take()
            .unwrap_or_else(|| self.options.workspace.clone());
        let task_name = request
            .task_name
            .as_deref()
            .map(str::trim)
            .filter(|task_name| !task_name.is_empty())
            .unwrap_or(agent_name);
        let mut task = task_from_definition_with_task_name(
            &self.effective_definition(definition),
            request.prompt,
            Some(workspace.as_path()),
            Some(task_name),
        );
        task.model = resolved_model_id.into();
        merge_request_metadata(&mut task, request.metadata);
        task
    }
}
