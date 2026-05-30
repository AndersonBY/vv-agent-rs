use crate::config::{apply_resolved_model_limits, ResolvedModelConfig};
use crate::llm::{LlmClient, ScriptedLlmClient};
use crate::runtime::AgentRuntime;
use crate::sdk::session::AgentSessionRunRequest;
use crate::sdk::types::{AgentDefinition, AgentRun, AgentSDKOptions};

use super::controls::run_controls_from_request;
use super::llm::build_llm_from_options;
use super::options::configure_runtime_from_options;
use crate::sdk::client::task::{merge_request_metadata, task_from_definition_with_task_name};

pub trait RunAgent {
    fn run(&self, definition: &AgentDefinition, prompt: String) -> Result<AgentRun, String> {
        self.run_with_session(definition, AgentSessionRunRequest::new(prompt))
    }

    fn run_with_session(
        &self,
        definition: &AgentDefinition,
        request: AgentSessionRunRequest,
    ) -> Result<AgentRun, String>;
}

impl<C: LlmClient + Clone + 'static> RunAgent for AgentRuntime<C> {
    fn run_with_session(
        &self,
        definition: &AgentDefinition,
        request: AgentSessionRunRequest,
    ) -> Result<AgentRun, String> {
        let controls = run_controls_from_request(&request);
        let workspace = request.workspace.clone();
        let mut task = task_from_definition_with_task_name(
            definition,
            request.prompt,
            workspace.as_deref(),
            request.task_name.as_deref(),
        );
        merge_request_metadata(&mut task, request.metadata);
        task.initial_messages = request.initial_messages;
        task.initial_shared_state = request.shared_state;
        let result = self
            .run_with_controls(task, controls)
            .map_err(|err| err.to_string())?;
        Ok(AgentRun {
            agent_name: definition.model.clone(),
            result,
            resolved: unresolved_definition_model(definition),
        })
    }
}

impl RunAgent for ScriptedLlmClient {
    fn run_with_session(
        &self,
        definition: &AgentDefinition,
        request: AgentSessionRunRequest,
    ) -> Result<AgentRun, String> {
        let runtime = AgentRuntime::new(self.clone());
        let controls = run_controls_from_request(&request);
        let workspace = request.workspace.clone();
        let mut task = task_from_definition_with_task_name(
            definition,
            request.prompt,
            workspace.as_deref(),
            request.task_name.as_deref(),
        );
        merge_request_metadata(&mut task, request.metadata);
        task.initial_messages = request.initial_messages;
        task.initial_shared_state = request.shared_state;
        runtime
            .run_with_controls(task, controls)
            .map_err(|err| err.to_string())
            .map(|result| AgentRun {
                agent_name: definition.model.clone(),
                resolved: unresolved_definition_model(definition),
                result,
            })
    }
}

#[derive(Clone)]
pub(in crate::sdk::client) struct SettingsRunAgent {
    pub(in crate::sdk::client) options: AgentSDKOptions,
}

impl RunAgent for SettingsRunAgent {
    fn run_with_session(
        &self,
        definition: &AgentDefinition,
        request: AgentSessionRunRequest,
    ) -> Result<AgentRun, String> {
        let backend = definition
            .backend
            .clone()
            .unwrap_or_else(|| self.options.default_backend.clone());
        let (llm, resolved) = build_llm_from_options(&self.options, &backend, &definition.model)?;
        let mut runtime = AgentRuntime::new(llm);
        configure_runtime_from_options(&mut runtime, &self.options);

        let controls = run_controls_from_request(&request);
        let effective_workspace = request
            .workspace
            .clone()
            .unwrap_or_else(|| self.options.workspace.clone());
        let mut task = task_from_definition_with_task_name(
            definition,
            request.prompt,
            Some(effective_workspace.as_path()),
            request.task_name.as_deref(),
        );
        task.model = resolved.model_id.clone();
        apply_resolved_model_limits(&mut task, &resolved);
        merge_request_metadata(&mut task, request.metadata);
        task.initial_messages = request.initial_messages;
        task.initial_shared_state = request.shared_state;
        let result = runtime
            .run_with_controls(task, controls)
            .map_err(|err| err.to_string())?;
        Ok(AgentRun {
            agent_name: definition.model.clone(),
            result,
            resolved,
        })
    }
}

fn unresolved_definition_model(definition: &AgentDefinition) -> ResolvedModelConfig {
    ResolvedModelConfig::new(
        definition
            .backend
            .clone()
            .unwrap_or_else(|| "moonshot".to_string()),
        definition.model.clone(),
        definition.model.clone(),
        definition.model.clone(),
        Vec::new(),
    )
}
