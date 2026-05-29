use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::config::{
    apply_resolved_model_limits, build_vv_llm_from_local_settings, ResolvedModelConfig,
};
use crate::llm::{LlmClient, ScriptedLlmClient};
use crate::runtime::{AgentRuntime, ExecutionContext, RuntimeRunControls};
use crate::workspace::{LocalWorkspaceBackend, WorkspaceBackend};

use super::super::session::AgentSessionRunRequest;
use super::super::types::{AgentDefinition, AgentRun, AgentSDKOptions, SdkLlmClient};
use super::task::{merge_request_metadata, task_from_definition_with_task_name};

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

fn execution_context_from_request(request: &AgentSessionRunRequest) -> Option<ExecutionContext> {
    request
        .stream_callback
        .clone()
        .map(|callback| ExecutionContext::default().with_stream_callback(callback))
}

pub(super) fn run_controls_from_request(request: &AgentSessionRunRequest) -> RuntimeRunControls {
    RuntimeRunControls {
        log_handler: request.runtime_event_handler.clone(),
        before_cycle_messages: request.before_cycle_messages.clone(),
        interruption_messages: request.interruption_messages.clone(),
        steering_queue: request.steering_queue.clone(),
        cancellation_token: request.cancellation_token.clone(),
        execution_context: execution_context_from_request(request),
        workspace: request.workspace.clone(),
        workspace_backend: request.workspace.as_ref().map(|workspace| {
            Arc::new(LocalWorkspaceBackend::new(workspace.clone())) as Arc<dyn WorkspaceBackend>
        }),
        sub_task_manager: request.sub_task_manager.clone(),
    }
}

#[derive(Clone)]
pub(super) struct SettingsRunAgent {
    pub(super) options: AgentSDKOptions,
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

fn build_llm_from_options(
    options: &AgentSDKOptions,
    backend: &str,
    model: &str,
) -> Result<(SdkLlmClient, ResolvedModelConfig), String> {
    if let Some(builder) = &options.llm_builder {
        let (mut llm, resolved) = builder(
            options.settings_file.as_path(),
            backend,
            model,
            options.timeout_seconds,
        )?;
        apply_debug_dump_dir_to_llm(&mut llm, options.debug_dump_dir.as_deref());
        return Ok((llm, resolved));
    }
    let (mut llm, resolved) = build_vv_llm_from_local_settings(
        &options.settings_file,
        backend,
        model,
        options.timeout_seconds,
    )
    .map_err(|err| err.to_string())?;
    if let Some(debug_dump_dir) = &options.debug_dump_dir {
        llm = llm.with_debug_dump_dir(debug_dump_dir);
    }
    Ok((Arc::new(llm), resolved))
}

fn apply_debug_dump_dir_to_llm(llm: &mut SdkLlmClient, debug_dump_dir: Option<&str>) {
    let Some(debug_dump_dir) = debug_dump_dir else {
        return;
    };
    let debug_dump_dir = Path::new(debug_dump_dir);
    if let Some(configured_llm) = llm.clone_with_debug_dump_dir(debug_dump_dir) {
        *llm = configured_llm;
    } else {
        llm.set_debug_dump_dir(debug_dump_dir);
    }
}

pub(super) fn configure_runtime_from_options<C: LlmClient + Clone + 'static>(
    runtime: &mut AgentRuntime<C>,
    options: &AgentSDKOptions,
) {
    if let Some(factory) = &options.tool_registry_factory {
        runtime.tool_registry = factory();
    }
    if let Some(execution_backend) = &options.execution_backend {
        runtime.execution_backend = execution_backend.clone();
    }
    if let Some(log_handler) = &options.log_handler {
        let option_handler = log_handler.clone();
        let previous_handler = runtime.log_handler.take();
        runtime.log_handler = Some(Arc::new(Mutex::new(Box::new(move |event, payload| {
            if let Some(previous_handler) = &previous_handler {
                if let Ok(mut previous_handler) = previous_handler.lock() {
                    previous_handler(event, payload);
                }
            }
            option_handler(event, payload);
        }))));
    }
    if runtime.log_preview_chars.is_none() {
        runtime.log_preview_chars = options.log_preview_chars;
    }
    if runtime.default_workspace.is_none() {
        let workspace = options.workspace.clone();
        runtime.default_workspace = Some(workspace.clone());
        runtime.workspace_backend = Arc::new(LocalWorkspaceBackend::new(workspace));
    }
    runtime.hooks.extend(options.runtime_hooks.clone());
}
