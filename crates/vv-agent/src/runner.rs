mod builder;
mod checkpoint_runtime;
mod event_stream;
mod handoff;
mod helpers;
mod producer;
mod resume;
mod run_single;
mod session_blocking;
mod support;
mod trace_lifecycle;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::broadcast;

use crate::agent::Agent;
use crate::approval::ApprovalBroker;
use crate::checkpoint::{CheckpointConfig, ResumePolicy};
use crate::config::apply_resolved_model_limits;
use crate::context::RunContext;
use crate::context_providers::{
    assemble_context_fragments, collect_context_fragments, ContextBundle, ContextRequest,
};
use crate::events::{AgentErrorPayload, RunEvent};
use crate::guardrails::GuardrailOutcome;
use crate::llm::LlmClient;
use crate::model::{ModelError, ModelProvider, ModelRef};
use crate::result::{RunResult, RunResumeContext};
use crate::run_config::{validate_max_cycles, RunConfig, INITIAL_BUDGET_USAGE_METADATA_KEY};
use crate::runtime::checkpoint_resume::{
    CheckpointController, CheckpointControllerRequest, CheckpointEventSink,
    CheckpointResumeController,
};
use crate::runtime::run_definition::{
    build_frozen_task, build_run_definition, frozen_definition_messages, RunDefinitionRequest,
};
use crate::runtime::state::Checkpoint;
use crate::runtime::tool_planner::project_tool_policy;
use crate::runtime::{
    AgentRuntime, CheckpointRuntimeControl, ExecutionContext, RuntimeRunControls,
};
use crate::sessions::{checkpoint_session_commit_id, session_commit_payload_digest, SessionItem};
use crate::tools::{ToolEnablementContext, ToolRegistry};
use crate::types::{AgentResult, AgentStatus, AgentTask, MessageRole};
use crate::workspace::LocalWorkspaceBackend;

pub use builder::RunnerBuilder;
use checkpoint_runtime::{
    prepare_checkpoint_resume, prepare_checkpoint_runtime, prepare_checkpoint_terminal,
    replay_checkpoint_terminal, CheckpointRuntimeRequest, CheckpointRuntimeState,
};
pub(crate) use event_stream::map_stream_event;
pub use event_stream::RunEventStream;
#[doc(hidden)]
pub use event_stream::{map_runtime_event, RuntimeEventContext};
use helpers::{
    effective_model_ref, effective_trace_id, effective_workflow_name, status_string, terminal_event,
};
pub(crate) use producer::CheckpointStartOutcome;
use session_blocking::block_on_session;
use support::{
    apply_cancellation_precedence, apply_input_guardrails, apply_optional_output_validation,
    apply_output_guardrails, capture_event, effective_event_store, effective_session_id,
    extract_handoff, initial_budget_usage, insert_context_metadata, merged_tool_policy,
    ApprovalHook, SingleRunOutcome,
};
use trace_lifecycle::RunTrace;

#[derive(Clone, Debug, PartialEq)]
pub struct NormalizedInput {
    pub text: String,
}

impl From<&str> for NormalizedInput {
    fn from(value: &str) -> Self {
        Self {
            text: value.to_string(),
        }
    }
}

impl From<String> for NormalizedInput {
    fn from(value: String) -> Self {
        Self { text: value }
    }
}

struct InstructionBuildRequest<'a> {
    agent: &'a Agent,
    run_context: &'a RunContext,
    input_text: &'a str,
    config: &'a RunConfig,
    model: &'a str,
    trace_id: &'a str,
    session: Option<Arc<dyn crate::sessions::Session>>,
    workspace: &'a std::path::Path,
}

pub(super) struct CheckpointAdmission {
    pub checkpoint: Checkpoint,
    pub terminal_replayed: bool,
}

pub(super) type CheckpointAdmissionSender = tokio::sync::oneshot::Sender<CheckpointAdmission>;

#[derive(Clone)]
pub struct Runner {
    model_provider: Arc<dyn ModelProvider>,
    workspace: PathBuf,
    tool_registry: ToolRegistry,
    default_run_config: RunConfig,
}

impl Runner {
    pub fn builder() -> RunnerBuilder {
        RunnerBuilder::default()
    }

    pub async fn run(
        &self,
        agent: &Agent,
        input: impl Into<NormalizedInput>,
    ) -> Result<RunResult, String> {
        self.run_with_config(agent, input, RunConfig::default())
            .await
    }

    pub async fn run_with_config(
        &self,
        agent: &Agent,
        input: impl Into<NormalizedInput>,
        config: RunConfig,
    ) -> Result<RunResult, String> {
        let runner = self.clone();
        let agent = agent.clone();
        let input = input.into();
        tokio::task::spawn_blocking(move || runner.run_blocking(&agent, input, config, None))
            .await
            .map_err(|error| format!("runner task failed: {error}"))?
    }

    async fn run_with_config_and_run_id(
        &self,
        agent: &Agent,
        input: NormalizedInput,
        config: RunConfig,
        run_id: String,
    ) -> Result<RunResult, String> {
        let runner = self.clone();
        let agent = agent.clone();
        tokio::task::spawn_blocking(move || {
            runner.run_agent_chain_with_initial(
                &agent,
                input,
                config,
                Some(Arc::new(Mutex::new(Vec::new()))),
                None,
                None,
                None,
                Some(run_id),
            )
        })
        .await
        .map_err(|error| format!("runner task failed: {error}"))?
    }

    pub fn run_blocking(
        &self,
        agent: &Agent,
        input: NormalizedInput,
        config: RunConfig,
        event_collector: Option<Arc<std::sync::Mutex<Vec<RunEvent>>>>,
    ) -> Result<RunResult, String> {
        self.run_blocking_with_event_sender(agent, input, config, event_collector, None, None)
    }

    fn run_blocking_with_event_sender(
        &self,
        agent: &Agent,
        input: NormalizedInput,
        config: RunConfig,
        event_collector: Option<Arc<std::sync::Mutex<Vec<RunEvent>>>>,
        event_sender: Option<broadcast::Sender<RunEvent>>,
        checkpoint_admission_sender: Option<CheckpointAdmissionSender>,
    ) -> Result<RunResult, String> {
        let event_collector =
            Some(event_collector.unwrap_or_else(|| Arc::new(std::sync::Mutex::new(Vec::new()))));
        self.run_agent_chain(
            agent,
            input,
            config,
            event_collector,
            event_sender,
            checkpoint_admission_sender,
        )
    }

    fn build_instructions_with_context(
        &self,
        request: InstructionBuildRequest<'_>,
    ) -> Result<(String, Option<ContextBundle>), String> {
        let InstructionBuildRequest {
            agent,
            run_context,
            input_text,
            config,
            model,
            trace_id,
            session,
            workspace,
        } = request;
        let providers = self
            .default_run_config
            .context_providers
            .iter()
            .chain(config.context_providers.iter())
            .cloned()
            .collect::<Vec<_>>();
        let mut request = ContextRequest::new(agent.name(), input_text)
            .model(model)
            .trace_id(trace_id)
            .workspace(workspace);
        if let Some(session) = session {
            request = request.session(session);
        }
        if let Some(context) = config
            .app_state
            .clone()
            .or_else(|| self.default_run_config.app_state.clone())
        {
            request = request.context(context);
        }
        request.metadata = agent.metadata().clone();
        request
            .metadata
            .extend(self.default_run_config.metadata.clone());
        request.metadata.extend(config.metadata.clone());
        if let Some(max_chars) = config
            .max_context_chars
            .or(self.default_run_config.max_context_chars)
        {
            request = request.max_prompt_chars(max_chars);
        }
        let mut fragments = vec![crate::context_providers::ContextFragment::new(
            "agent_instructions",
            agent.resolve_instructions(run_context),
        )
        .stable(true)
        .priority(0)
        .source("agent.instructions")];
        if !agent.sub_agents().is_empty() {
            let available_sub_agents = agent
                .sub_agents()
                .iter()
                .map(|(id, config)| (id.clone(), config.description.clone()))
                .collect();
            fragments.push(
                crate::context_providers::ContextFragment::new(
                    "configured_sub_agents",
                    crate::prompt::templates::render_sub_agents("en-US", &available_sub_agents),
                )
                .stable(true)
                .priority(10)
                .source("agent.sub_agents"),
            );
        }
        fragments.extend(
            collect_context_fragments(&request, &providers)
                .map_err(|error| format!("context provider failed: {error}"))?,
        );
        let bundle = assemble_context_fragments(&request, fragments)
            .map_err(|error| format!("context assembly failed: {error}"))?;
        Ok((bundle.prompt.clone(), Some(bundle)))
    }
}

#[derive(Clone)]
struct ArcLlmClient(Arc<dyn LlmClient>);

impl LlmClient for ArcLlmClient {
    fn complete(
        &self,
        request: crate::llm::LlmRequest,
    ) -> Result<crate::types::LLMResponse, crate::llm::LlmError> {
        self.0.complete(request)
    }

    fn complete_with_stream(
        &self,
        request: crate::llm::LlmRequest,
        stream_callback: Option<crate::llm::LlmStreamCallback>,
    ) -> Result<crate::types::LLMResponse, crate::llm::LlmError> {
        self.0.complete_with_stream(request, stream_callback)
    }
}

fn preload_checkpoint(config: Option<&CheckpointConfig>) -> Result<Option<Checkpoint>, String> {
    let Some(config) = config else {
        return Ok(None);
    };
    config.validate().map_err(|error| error.to_string())?;
    if config.resume_policy == ResumePolicy::New {
        return Ok(None);
    }
    let store = config.store.as_ref().ok_or_else(|| {
        "checkpoint_store_unavailable: process-local Runner resume requires CheckpointConfig.store"
            .to_string()
    })?;
    let key = config.key.as_deref().ok_or_else(|| {
        "checkpoint_key_required: resume_if_present and require_existing need an explicit key"
            .to_string()
    })?;
    store
        .load_checkpoint(key)
        .map_err(|error| error.to_string())
}

fn format_model_error(error: ModelError) -> String {
    error.to_string()
}
