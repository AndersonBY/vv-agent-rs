mod builder;
mod event_stream;
mod handoff;
mod helpers;
mod producer;
mod resume;
mod session_blocking;
mod support;
mod trace_lifecycle;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::broadcast;

use crate::agent::Agent;
use crate::approval::ApprovalBroker;
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
use crate::runtime::tool_planner::project_tool_policy;
use crate::runtime::{AgentRuntime, ExecutionContext, RuntimeRunControls};
use crate::sessions::SessionItem;
use crate::tools::{ToolEnablementContext, ToolRegistry};
use crate::types::{AgentResult, AgentStatus, AgentTask, MessageRole};
use crate::workspace::LocalWorkspaceBackend;

pub use builder::RunnerBuilder;
use event_stream::map_stream_event;
pub use event_stream::RunEventStream;
#[doc(hidden)]
pub use event_stream::{map_runtime_event, RuntimeEventContext};
use helpers::{
    effective_model_ref, effective_trace_id, effective_workflow_name, is_runtime_terminal_log,
    status_string, terminal_event,
};
use session_blocking::block_on_session;
use support::{
    apply_cancellation_precedence, apply_input_guardrails, apply_output_guardrails, capture_event,
    effective_event_store, effective_session_id, extract_handoff, initial_budget_usage,
    insert_context_metadata, merged_tool_policy, ApprovalHook, SingleRunOutcome,
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

    pub fn run_blocking(
        &self,
        agent: &Agent,
        input: NormalizedInput,
        config: RunConfig,
        event_collector: Option<Arc<std::sync::Mutex<Vec<RunEvent>>>>,
    ) -> Result<RunResult, String> {
        self.run_blocking_with_event_sender(agent, input, config, event_collector, None)
    }

    fn run_blocking_with_event_sender(
        &self,
        agent: &Agent,
        input: NormalizedInput,
        config: RunConfig,
        event_collector: Option<Arc<std::sync::Mutex<Vec<RunEvent>>>>,
        event_sender: Option<broadcast::Sender<RunEvent>>,
    ) -> Result<RunResult, String> {
        let event_collector =
            Some(event_collector.unwrap_or_else(|| Arc::new(std::sync::Mutex::new(Vec::new()))));
        self.run_agent_chain(agent, input, config, event_collector, event_sender)
    }

    fn run_single_agent(
        &self,
        agent: &Agent,
        input: NormalizedInput,
        config: RunConfig,
        event_collector: Option<Arc<std::sync::Mutex<Vec<RunEvent>>>>,
        event_sender: Option<broadcast::Sender<RunEvent>>,
    ) -> Result<SingleRunOutcome, String> {
        let max_cycles = validate_max_cycles(
            config
                .max_cycles
                .or(self.default_run_config.max_cycles)
                .or(agent.max_cycles())
                .unwrap_or(10),
        )?;
        let no_tool_policy = config
            .no_tool_policy
            .or(self.default_run_config.no_tool_policy)
            .or(agent.no_tool_policy())
            .unwrap_or_default();
        let provider = config
            .model_provider
            .clone()
            .or_else(|| self.default_run_config.model_provider.clone())
            .unwrap_or_else(|| self.model_provider.clone());
        let model_ref = effective_model_ref(agent, &self.default_run_config, &config, &provider)
            .ok_or_else(|| "agent model is not configured".to_string())?;
        let (event_store, event_store_fail_closed) =
            effective_event_store(&self.default_run_config, &config);
        let workspace = config
            .workspace
            .clone()
            .or_else(|| self.default_run_config.workspace.clone())
            .unwrap_or_else(|| self.workspace.clone());
        let session = config
            .session
            .clone()
            .or_else(|| self.default_run_config.session.clone());
        let tool_policy = merged_tool_policy(
            agent.tool_policy(),
            &self.default_run_config.tool_policy,
            &config.tool_policy,
        );
        let approval_provider = config
            .approval_provider
            .clone()
            .or_else(|| self.default_run_config.approval_provider.clone());
        let approval_broker = config
            .approval_broker
            .clone()
            .or_else(|| self.default_run_config.approval_broker.clone())
            .or_else(|| {
                approval_provider
                    .as_ref()
                    .map(|_| ApprovalBroker::default())
            });
        let approval_timeout = config
            .approval_timeout
            .or(self.default_run_config.approval_timeout);
        let cancellation_token = config
            .cancellation_token
            .clone()
            .or_else(|| self.default_run_config.cancellation_token.clone());
        let budget_limits = config
            .budget_limits
            .clone()
            .or_else(|| self.default_run_config.budget_limits.clone());
        let host_cost_meter = config
            .host_cost_meter
            .clone()
            .or_else(|| self.default_run_config.host_cost_meter.clone());
        let initial_budget_usage =
            initial_budget_usage(&self.default_run_config.metadata, &config.metadata)?;
        let memory_providers = self
            .default_run_config
            .memory_providers
            .iter()
            .chain(config.memory_providers.iter())
            .cloned()
            .collect::<Vec<_>>();
        let run_id = format!("run_{}", uuid::Uuid::new_v4().simple());
        let mut run_metadata = agent.metadata().clone();
        run_metadata.extend(self.default_run_config.metadata.clone());
        run_metadata.extend(config.metadata.clone());
        run_metadata.remove(INITIAL_BUDGET_USAGE_METADATA_KEY);
        let trace_id = effective_trace_id(&self.default_run_config, &config, &run_metadata);
        let workflow_name = effective_workflow_name(&self.default_run_config, &config);
        run_metadata.insert("trace_id".to_string(), Value::String(trace_id.clone()));
        let app_state = config
            .app_state
            .clone()
            .or_else(|| self.default_run_config.app_state.clone());
        let mut run_context = RunContext {
            run_id: run_id.clone(),
            agent_name: agent.name().to_string(),
            model: Some(model_ref.clone()),
            workspace: Some(workspace.clone()),
            metadata: run_metadata,
            app_state: app_state.clone(),
        };
        let trace_sink = config
            .trace_sink
            .clone()
            .or_else(|| self.default_run_config.trace_sink.clone());
        let trace = RunTrace::start(
            trace_sink.clone(),
            &trace_id,
            &run_context.run_id,
            agent.name(),
            workflow_name.as_deref(),
        );
        let event_session_id = session
            .as_ref()
            .map(|session| session.session_id().trim())
            .filter(|session_id| !session_id.is_empty())
            .map(str::to_string)
            .or_else(|| {
                run_context
                    .metadata
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|session_id| !session_id.is_empty())
                    .map(str::to_string)
            });
        let original_input = input.text.clone();
        let input_text = match apply_input_guardrails(agent, &run_context, input) {
            GuardrailOutcome::Allow(input) => input.text,
            GuardrailOutcome::Block { message } | GuardrailOutcome::RequireApproval { message } => {
                let mut failed_event = RunEvent::run_failed(
                    &run_context.run_id,
                    &trace_id,
                    agent.name(),
                    AgentErrorPayload::new(&message),
                )
                .with_completion_details(
                    Some(crate::types::CompletionReason::Failed),
                    None,
                    None,
                );
                if let Some(session_id) = event_session_id.as_ref() {
                    failed_event = failed_event.with_session_id(session_id);
                }
                capture_event(
                    event_collector.as_ref(),
                    event_sender.as_ref(),
                    event_store.as_ref(),
                    event_store_fail_closed,
                    failed_event,
                )?;
                let ended_run_span =
                    trace.finish("failed", Some(("error", Value::String(message.clone()))));
                let events = event_collector
                    .as_ref()
                    .and_then(|collector| collector.lock().ok().map(|events| events.clone()))
                    .unwrap_or_default();
                let mut result_metadata = run_context.metadata.clone();
                result_metadata.insert(
                    "run_span".to_string(),
                    serde_json::to_value(ended_run_span)
                        .unwrap_or_else(|error| Value::String(error.to_string())),
                );
                return Ok(SingleRunOutcome {
                    result: RunResult::without_resolved_model(
                        agent.name().to_string(),
                        AgentResult::failed(message),
                    )
                    .with_ids(&run_context.run_id, &trace_id)
                    .with_input(original_input)
                    .with_events(events)
                    .with_metadata(result_metadata),
                    handoff: None,
                });
            }
        };
        let resolved = provider.resolve(&model_ref).map_err(format_model_error)?;
        run_context.model = Some(ModelRef::named(resolved.model_id.clone()));
        let tool_enablement_context =
            ToolEnablementContext::new(run_context.clone()).with_app_state(app_state.clone());
        let mut llm = provider.client(&resolved).map_err(format_model_error)?;
        if let Some(debug_dump_dir) = config
            .debug_dump_dir
            .as_ref()
            .or(self.default_run_config.debug_dump_dir.as_ref())
        {
            llm = llm
                .clone_with_debug_dump_dir(debug_dump_dir)
                .ok_or_else(|| {
                    "configured LLM client does not support debug_dump_dir".to_string()
                })?;
        }
        let provider_settings = provider.default_settings(&resolved);
        let settings = provider_settings
            .merge(
                self.default_run_config
                    .model_settings
                    .as_ref()
                    .unwrap_or(&crate::model_settings::ModelSettings::default()),
            )
            .merge(agent.model_settings())
            .merge(
                config
                    .model_settings
                    .as_ref()
                    .unwrap_or(&crate::model_settings::ModelSettings::default()),
            );
        settings
            .validate()
            .map_err(|error| format!("invalid model settings: {error}"))?;
        let session_items = if let Some(session) = session.as_ref() {
            block_on_session(session.get_items(None))?
        } else {
            Vec::new()
        };
        let event_context = RuntimeEventContext::new(
            &run_context.run_id,
            &trace_id,
            agent.name(),
            event_session_id.clone(),
            &input_text,
        );
        let (instructions, context_bundle) =
            self.build_instructions_with_context(InstructionBuildRequest {
                agent,
                run_context: &run_context,
                input_text: &input_text,
                config: &config,
                model: &resolved.model_id,
                trace_id: &trace_id,
                session: session.clone(),
                workspace: &workspace,
            })?;
        let mut task = AgentTask::new(
            run_id,
            resolved.model_id.clone(),
            instructions,
            input_text.clone(),
        );
        task.max_cycles = max_cycles;
        task.no_tool_policy = no_tool_policy;
        task.has_sub_agents = false;
        task.sub_agents = agent.sub_agents().clone();
        task.metadata = agent.metadata().clone();
        task.metadata
            .extend(self.default_run_config.metadata.clone());
        task.metadata.extend(config.metadata.clone());
        task.metadata.remove(INITIAL_BUDGET_USAGE_METADATA_KEY);
        task.metadata
            .entry("agent_name".to_string())
            .or_insert_with(|| Value::String(agent.name().to_string()));
        task.metadata
            .insert("trace_id".to_string(), Value::String(trace_id.clone()));
        task.metadata.insert(
            "_vv_agent_run_id".to_string(),
            Value::String(run_context.run_id.clone()),
        );
        task.metadata.insert(
            "_vv_agent_trace_id".to_string(),
            Value::String(trace_id.clone()),
        );
        task.metadata.insert(
            "_vv_agent_agent_name".to_string(),
            Value::String(agent.name().to_string()),
        );
        task.metadata.insert(
            "_vv_agent_input".to_string(),
            Value::String(input_text.clone()),
        );
        if let Some(session_id) = event_session_id.as_ref() {
            task.metadata.insert(
                "_vv_agent_session_id".to_string(),
                Value::String(session_id.clone()),
            );
        }
        match agent.tool_use_behavior() {
            crate::agent::ToolUseBehavior::RunLlmAgain => {
                task.metadata.insert(
                    "_vv_agent_tool_use_behavior".to_string(),
                    Value::String("run_llm_again".to_string()),
                );
            }
            crate::agent::ToolUseBehavior::StopOnFirstTool => {
                task.metadata.insert(
                    "_vv_agent_tool_use_behavior".to_string(),
                    Value::String("stop_on_first_tool".to_string()),
                );
            }
            crate::agent::ToolUseBehavior::StopAtToolNames(names) => {
                task.metadata.insert(
                    "_vv_agent_tool_use_behavior".to_string(),
                    Value::String("stop_at_tool_names".to_string()),
                );
                task.metadata.insert(
                    "_vv_agent_stop_at_tool_names".to_string(),
                    Value::Array(names.iter().cloned().map(Value::String).collect()),
                );
            }
        }
        if let Some(context_bundle) = context_bundle {
            insert_context_metadata(&mut task.metadata, &context_bundle);
        }
        task.model_settings = Some(settings.clone());
        task.initial_shared_state = self.default_run_config.initial_shared_state.clone();
        task.initial_shared_state
            .extend(config.initial_shared_state.clone());
        project_tool_policy(&mut task, &tool_policy);
        apply_resolved_model_limits(&mut task, &resolved);
        task.initial_messages = config
            .initial_messages
            .clone()
            .or_else(|| self.default_run_config.initial_messages.clone())
            .unwrap_or_else(|| {
                session_items
                    .iter()
                    .map(SessionItem::to_message)
                    .collect::<Vec<_>>()
            });
        let session_result_prefix_len = task.initial_messages.len()
            + usize::from(
                task.initial_messages
                    .first()
                    .is_none_or(|message| message.role != MessageRole::System),
            );
        let mut registry = config
            .tool_registry_factory
            .as_ref()
            .or(self.default_run_config.tool_registry_factory.as_ref())
            .map(|factory| factory())
            .unwrap_or_else(|| self.tool_registry.clone());
        for tool_name in registry.list_planner_extra_tool_names() {
            if !task.extra_tool_names.contains(&tool_name) {
                task.extra_tool_names.push(tool_name);
            }
        }
        for handoff in agent.handoffs() {
            let spec = handoff.as_tool_spec(agent.name());
            task.extra_tool_names.push(spec.name.clone());
            registry.register(spec)?;
        }
        for tool in agent.tools() {
            if !tool.is_enabled(&tool_enablement_context) {
                continue;
            }
            let spec = tool.as_tool_spec();
            if spec.exposure != crate::tools::ToolExposure::Hidden {
                task.extra_tool_names.push(spec.name.clone());
            }
            registry.register(spec)?;
        }
        let pending_tool_approval = Arc::new(Mutex::new(None));
        let mut runtime = AgentRuntime::new(ArcLlmClient(llm))
            .with_tool_registry(registry)
            .with_settings_file("__runner_model_provider__")
            .with_default_backend(resolved.backend.clone())
            .with_tool_policy(tool_policy.clone())
            .with_pending_tool_approval(pending_tool_approval.clone());
        if let Some(log_preview_chars) = config
            .log_preview_chars
            .or(self.default_run_config.log_preview_chars)
        {
            runtime.log_preview_chars = Some(log_preview_chars);
        }
        runtime.default_workspace = Some(workspace.clone());
        runtime.workspace_backend = config
            .workspace_backend
            .clone()
            .or_else(|| self.default_run_config.workspace_backend.clone())
            .unwrap_or_else(|| Arc::new(LocalWorkspaceBackend::new(workspace.clone())));
        if let Some(execution_backend) = config
            .execution_backend
            .clone()
            .or_else(|| self.default_run_config.execution_backend.clone())
        {
            runtime.execution_backend = execution_backend;
        }
        runtime.hooks.extend(agent.hooks().iter().cloned());
        runtime
            .hooks
            .extend(self.default_run_config.hooks.iter().cloned());
        runtime.hooks.extend(config.hooks.iter().cloned());
        runtime
            .hooks
            .push(Arc::new(ApprovalHook::new(tool_policy.clone())));
        let has_event_sink = event_collector.is_some()
            || event_sender.is_some()
            || event_store.is_some()
            || trace.is_enabled();
        let event_store_error = Arc::new(Mutex::new(None::<String>));
        let runtime_log_handler = config
            .runtime_log_handler
            .clone()
            .or_else(|| self.default_run_config.runtime_log_handler.clone());
        let log_handler = if has_event_sink || runtime_log_handler.is_some() {
            let collector = event_collector.clone();
            let event_sender = event_sender.clone();
            let event_store = event_store.clone();
            let event_context = event_context.clone();
            let event_store_error = event_store_error.clone();
            let trace_observer = trace.observer();
            let configured_runtime_log_handler = runtime_log_handler.clone();
            Some(Arc::new(
                move |event: &str, payload: &std::collections::BTreeMap<String, Value>| {
                    if has_event_sink {
                        trace_observer.on_event(event, payload);
                        if !is_runtime_terminal_log(event)
                            && event_store_error
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .is_none()
                        {
                            if let Some(mapped) = map_runtime_event(event, payload, &event_context)
                            {
                                if let Err(error) = capture_event(
                                    collector.as_ref(),
                                    event_sender.as_ref(),
                                    event_store.as_ref(),
                                    event_store_fail_closed,
                                    mapped,
                                ) {
                                    *event_store_error
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                        Some(error);
                                }
                            }
                        }
                    }
                    if let Some(handler) = configured_runtime_log_handler.as_ref() {
                        handler(event, payload);
                    }
                },
            ) as crate::runtime::RuntimeEventHandler)
        } else {
            None
        };
        let runtime_stream_callback = config
            .runtime_stream_callback
            .clone()
            .or_else(|| self.default_run_config.runtime_stream_callback.clone());
        let stream_callback = if has_event_sink || runtime_stream_callback.is_some() {
            let collector = event_collector.clone();
            let event_sender = event_sender.clone();
            let event_store = event_store.clone();
            let event_context = event_context.clone();
            let event_store_error = event_store_error.clone();
            let configured_runtime_stream_callback = runtime_stream_callback.clone();
            Some(
                Arc::new(move |payload: &std::collections::BTreeMap<String, Value>| {
                    if has_event_sink
                        && event_store_error
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .is_none()
                    {
                        if let Some(mapped) = map_stream_event(payload, &event_context) {
                            if let Err(error) = capture_event(
                                collector.as_ref(),
                                event_sender.as_ref(),
                                event_store.as_ref(),
                                event_store_fail_closed,
                                mapped,
                            ) {
                                *event_store_error
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                    Some(error);
                            }
                        }
                    }
                    if let Some(callback) = configured_runtime_stream_callback.as_ref() {
                        callback(payload);
                    }
                }) as crate::llm::LlmStreamCallback,
            )
        } else {
            None
        };
        let mut background_parent_run_config = config.clone();
        background_parent_run_config.model = Some(model_ref.clone());
        background_parent_run_config.model_provider = Some(provider.clone());
        background_parent_run_config.model_settings = Some(settings.clone());
        background_parent_run_config.workspace = Some(workspace.clone());
        background_parent_run_config.workspace_backend = Some(runtime.workspace_backend.clone());
        background_parent_run_config.session = session.clone();
        background_parent_run_config.initial_messages = Some(task.initial_messages.clone());
        background_parent_run_config.max_cycles = Some(max_cycles);
        background_parent_run_config.max_handoffs = Some(
            config
                .max_handoffs
                .or(self.default_run_config.max_handoffs)
                .unwrap_or(10),
        );
        background_parent_run_config.tool_policy = tool_policy.clone();
        background_parent_run_config.execution_backend = Some(runtime.execution_backend.clone());
        background_parent_run_config.cancellation_token = cancellation_token.clone();
        background_parent_run_config.hooks = self
            .default_run_config
            .hooks
            .iter()
            .chain(config.hooks.iter())
            .cloned()
            .collect();
        background_parent_run_config.trace_sink = trace_sink;
        background_parent_run_config.trace_id = Some(trace_id.clone());
        background_parent_run_config.workflow_name = workflow_name.clone();
        background_parent_run_config.event_store = event_store.clone();
        background_parent_run_config.event_store_fail_closed = event_store_fail_closed;
        background_parent_run_config.approval_provider = approval_provider.clone();
        background_parent_run_config.approval_timeout = approval_timeout;
        background_parent_run_config.approval_broker = approval_broker.clone();
        background_parent_run_config.context_providers = self
            .default_run_config
            .context_providers
            .iter()
            .chain(config.context_providers.iter())
            .cloned()
            .collect();
        background_parent_run_config.max_context_chars = config
            .max_context_chars
            .or(self.default_run_config.max_context_chars);
        background_parent_run_config.memory_providers = memory_providers.clone();
        background_parent_run_config.app_state = app_state.clone();
        background_parent_run_config.initial_shared_state =
            self.default_run_config.initial_shared_state.clone();
        background_parent_run_config
            .initial_shared_state
            .extend(config.initial_shared_state.clone());
        background_parent_run_config.tool_registry_factory = config
            .tool_registry_factory
            .clone()
            .or_else(|| self.default_run_config.tool_registry_factory.clone());
        background_parent_run_config.log_preview_chars = config
            .log_preview_chars
            .or(self.default_run_config.log_preview_chars);
        background_parent_run_config.debug_dump_dir = config
            .debug_dump_dir
            .clone()
            .or_else(|| self.default_run_config.debug_dump_dir.clone());
        background_parent_run_config.before_cycle_messages = config
            .before_cycle_messages
            .clone()
            .or_else(|| self.default_run_config.before_cycle_messages.clone());
        background_parent_run_config.interruption_messages = config
            .interruption_messages
            .clone()
            .or_else(|| self.default_run_config.interruption_messages.clone());
        background_parent_run_config.sub_task_manager = config
            .sub_task_manager
            .clone()
            .or_else(|| self.default_run_config.sub_task_manager.clone());
        background_parent_run_config.runtime_log_handler = runtime_log_handler;
        background_parent_run_config.runtime_stream_callback = runtime_stream_callback;
        background_parent_run_config.budget_limits = budget_limits.clone();
        background_parent_run_config.host_cost_meter = host_cost_meter.clone();
        background_parent_run_config.metadata = self.default_run_config.metadata.clone();
        background_parent_run_config
            .metadata
            .extend(config.metadata.clone());
        background_parent_run_config
            .metadata
            .remove(INITIAL_BUDGET_USAGE_METADATA_KEY);
        let controls = RuntimeRunControls {
            log_handler,
            before_cycle_messages: config
                .before_cycle_messages
                .clone()
                .or_else(|| self.default_run_config.before_cycle_messages.clone()),
            interruption_messages: config
                .interruption_messages
                .clone()
                .or_else(|| self.default_run_config.interruption_messages.clone()),
            cancellation_token: cancellation_token.clone(),
            execution_context: Some(ExecutionContext {
                stream_callback,
                metadata: task.metadata.clone(),
                approval_provider,
                approval_broker,
                approval_timeout,
                memory_providers,
                app_state,
                ..ExecutionContext::default()
            }),
            workspace: Some(workspace),
            workspace_backend: runtime.workspace_backend.clone().into(),
            model_provider: Some(provider.clone()),
            run_context: Some(run_context.clone()),
            background_parent_run_config: Some(background_parent_run_config),
            sub_task_manager: config
                .sub_task_manager
                .clone()
                .or_else(|| self.default_run_config.sub_task_manager.clone()),
            budget_limits,
            host_cost_meter,
            initial_budget_usage,
            ..RuntimeRunControls::default()
        };
        let result = runtime
            .run_with_controls(task, controls)
            .map_err(|error| error.to_string())?;
        if let Some(error) = event_store_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            return Err(error);
        }
        let result = apply_output_guardrails(agent, &run_context, result);
        let result = apply_cancellation_precedence(result, cancellation_token.as_ref());
        let output_validation_error = if result.status == AgentStatus::Completed {
            result.final_answer.as_deref().and_then(|output| {
                agent.validate_output(output).err().map(|error| {
                    format!(
                        "failed to validate final output for agent `{}` as `{}`: {error}",
                        agent.name(),
                        agent.output_type_name().unwrap_or("configured output type")
                    )
                })
            })
        } else {
            None
        };
        let handoff = extract_handoff(&result);
        let new_items = result
            .messages
            .get(session_result_prefix_len..)
            .unwrap_or_default()
            .to_vec();
        if let Some(session) = session.as_ref() {
            let approval_call_ids = result
                .cycles
                .iter()
                .flat_map(|cycle| cycle.tool_results.iter())
                .filter(|tool_result| {
                    tool_result.error_code.as_deref() == Some("tool_approval_required")
                })
                .map(|tool_result| tool_result.tool_call_id.as_str())
                .collect::<std::collections::BTreeSet<_>>();
            let session_items = new_items
                .iter()
                .filter(|message| {
                    !(result.status == AgentStatus::WaitUser
                        && message.role == crate::types::MessageRole::Tool
                        && message
                            .tool_call_id
                            .as_deref()
                            .is_some_and(|call_id| approval_call_ids.contains(call_id)))
                })
                .filter_map(SessionItem::from_message)
                .collect::<Vec<_>>();
            block_on_session(session.add_items(session_items))?;
            if has_event_sink {
                let payload = std::collections::BTreeMap::from([(
                    "session_id".to_string(),
                    Value::String(session.session_id().to_string()),
                )]);
                if let Some(event) =
                    map_runtime_event("session_persisted", &payload, &event_context)
                {
                    capture_event(
                        event_collector.as_ref(),
                        event_sender.as_ref(),
                        event_store.as_ref(),
                        event_store_fail_closed,
                        event,
                    )?;
                }
            }
        }
        capture_event(
            event_collector.as_ref(),
            event_sender.as_ref(),
            event_store.as_ref(),
            event_store_fail_closed,
            terminal_event(
                &result,
                &run_context.run_id,
                &trace_id,
                agent.name(),
                event_session_id.as_deref(),
                cancellation_token.as_ref(),
            ),
        )?;
        let ended_run_span = if let Some(error) = output_validation_error.as_ref() {
            trace.finish("failed", Some(("error", Value::String(error.clone()))))
        } else {
            trace.finish(
                &status_string(result.status),
                result
                    .final_answer
                    .clone()
                    .or_else(|| result.wait_reason.clone())
                    .or_else(|| result.error.clone())
                    .map(|output| ("final_output", Value::String(output))),
            )
        };
        let events = event_collector
            .as_ref()
            .and_then(|collector| collector.lock().ok().map(|events| events.clone()))
            .unwrap_or_default();
        let mut result_metadata = run_context.metadata.clone();
        result_metadata.insert(
            "resolved_model".to_string(),
            Value::String(resolved.model_id.clone()),
        );
        result_metadata.insert(
            "backend".to_string(),
            Value::String(resolved.backend.clone()),
        );
        result_metadata.insert(
            "run_span".to_string(),
            serde_json::to_value(ended_run_span)
                .unwrap_or_else(|error| Value::String(error.to_string())),
        );
        let pending_tool_approval = pending_tool_approval
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(error) = output_validation_error {
            return Err(error);
        }
        Ok(SingleRunOutcome {
            result: RunResult::new(agent.name().to_string(), result, resolved)
                .with_ids(&run_context.run_id, &trace_id)
                .with_input(&original_input)
                .with_new_items(new_items)
                .with_events(events)
                .with_metadata(result_metadata)
                .with_resume_context(RunResumeContext {
                    agent: agent.clone(),
                    input: NormalizedInput {
                        text: original_input.clone(),
                    },
                    config,
                    runner: self.clone(),
                    pending_tool_approval,
                }),
            handoff,
        })
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

fn format_model_error(error: ModelError) -> String {
    error.to_string()
}
