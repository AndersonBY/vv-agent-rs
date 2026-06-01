mod event_stream;
mod session_blocking;

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::agent::Agent;
use crate::config::apply_resolved_model_limits;
use crate::context::RunContext;
use crate::events::RunEvent;
use crate::guardrails::GuardrailOutcome;
use crate::llm::LlmClient;
use crate::model::{ModelError, ModelProvider, VvLlmModelProvider};
use crate::result::{RunResult, RunResumeContext, RunState};
use crate::run_config::RunConfig;
use crate::runtime::{
    AgentRuntime, BeforeToolCallEvent, BeforeToolCallPatch, ExecutionContext, RuntimeHook,
    RuntimeRunControls,
};
use crate::sessions::SessionItem;
use crate::tools::{ApprovalPolicy, ToolPolicy, ToolRegistry};
use crate::tracing::Span;
use crate::types::{
    AgentResult, AgentTask, NoToolPolicy, ToolDirective, ToolExecutionResult, ToolResultStatus,
};
use crate::workspace::LocalWorkspaceBackend;

use event_stream::map_runtime_event;
pub use event_stream::RunEventStream;
use session_blocking::block_on_session;

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
        self.run_blocking(agent, input.into(), config, None)
    }

    pub async fn resume(&self, state: RunState) -> Result<RunResult, String> {
        let (source, approved_ids) = state.into_inner();
        let Some(resume_context) = source.resume_context().cloned() else {
            return Err("run state does not include resume context".to_string());
        };
        if let Some(result) =
            self.resume_approved_tool_call(&source, &resume_context, &approved_ids)
        {
            return result;
        }
        let mut config = resume_context.config;
        config.metadata.insert(
            "approved_tool_interruption_ids".to_string(),
            Value::Array(approved_ids.iter().cloned().map(Value::String).collect()),
        );
        let mut result = self
            .run_with_config(&resume_context.agent, resume_context.input, config)
            .await
            .map_err(|error| format!("resume failed: {error}"))?;
        if result.status() == crate::types::AgentStatus::MaxCycles {
            result = completed_from_first_tool_result(result);
        }
        Ok(result)
    }

    fn resume_approved_tool_call(
        &self,
        source: &RunResult,
        resume_context: &RunResumeContext,
        approved_ids: &[String],
    ) -> Option<Result<RunResult, String>> {
        let approval = find_approved_tool_call(source.result(), approved_ids)?;
        let mut registry = self.tool_registry.clone();
        for handoff in resume_context.agent.handoffs() {
            if let Err(error) = registry.register(handoff.as_tool_spec(resume_context.agent.name()))
            {
                return Some(Err(error));
            }
        }
        for tool in resume_context.agent.tools() {
            if let Err(error) = registry.register(tool.as_tool_spec()) {
                return Some(Err(error));
            }
        }
        let workspace = resume_context
            .config
            .workspace
            .clone()
            .or_else(|| self.default_run_config.workspace.clone())
            .unwrap_or_else(|| self.workspace.clone());
        let workspace_backend = resume_context
            .config
            .workspace_backend
            .clone()
            .or_else(|| self.default_run_config.workspace_backend.clone())
            .unwrap_or_else(|| Arc::new(LocalWorkspaceBackend::new(workspace.clone())));
        let mut context = crate::tools::ToolContext {
            workspace: workspace.clone(),
            shared_state: source.result().shared_state.clone(),
            cycle_index: approval.cycle_index,
            task_id: format!("{}_run", resume_context.agent.name()),
            metadata: resume_context.agent.metadata().clone(),
            workspace_backend,
            model_provider: Some(self.model_provider.clone()),
            sub_task_runner: None,
            sub_task_manager: None,
            execution_backend: None,
        };
        context
            .metadata
            .extend(resume_context.config.metadata.clone());
        context
            .metadata
            .entry("agent_name".to_string())
            .or_insert_with(|| Value::String(resume_context.agent.name().to_string()));
        let tool_result = crate::tools::dispatch_tool_call(&registry, &mut context, &approval.call);
        if tool_result.status != ToolResultStatus::Success {
            return Some(Err(tool_result.content));
        }
        let mut agent_result = source.result().clone();
        agent_result.status = crate::types::AgentStatus::Completed;
        agent_result.final_answer = Some(tool_result.content.clone());
        if let Some(cycle) = agent_result
            .cycles
            .iter_mut()
            .find(|cycle| cycle.index == approval.cycle_index)
        {
            cycle.tool_results.push(tool_result.clone());
        }
        agent_result.messages.push(tool_result.to_message());
        Some(Ok(RunResult::new(
            resume_context.agent.name().to_string(),
            agent_result,
            source.resolved_model().clone(),
        )
        .with_resume_context(resume_context.clone())))
    }

    pub fn run_blocking(
        &self,
        agent: &Agent,
        input: NormalizedInput,
        config: RunConfig,
        event_collector: Option<Arc<std::sync::Mutex<Vec<RunEvent>>>>,
    ) -> Result<RunResult, String> {
        self.run_agent_chain(agent, input, config, event_collector)
    }

    fn run_agent_chain(
        &self,
        agent: &Agent,
        input: NormalizedInput,
        config: RunConfig,
        event_collector: Option<Arc<std::sync::Mutex<Vec<RunEvent>>>>,
    ) -> Result<RunResult, String> {
        let mut current_agent = agent.clone();
        let mut current_input = input;
        for _ in 0..=max_handoff_depth(&config, &current_agent) {
            let outcome = self.run_single_agent(
                &current_agent,
                current_input.clone(),
                config.clone(),
                event_collector.clone(),
            )?;
            let Some(handoff) = outcome.handoff else {
                return Ok(outcome.result);
            };
            let target = current_agent
                .handoffs()
                .iter()
                .find(|candidate| candidate.target().name() == handoff.to_agent)
                .map(|candidate| candidate.target().clone())
                .ok_or_else(|| {
                    format!(
                        "handoff target `{}` is not registered on agent `{}`",
                        handoff.to_agent,
                        current_agent.name()
                    )
                })?;
            push_event(
                event_collector.as_ref(),
                RunEvent::handoff_completed(
                    format!("{}_run", handoff.from_agent),
                    format!("{}_run", handoff.from_agent),
                    handoff.from_agent.clone(),
                    handoff.to_agent.clone(),
                    "",
                ),
            );
            current_input = NormalizedInput {
                text: handoff.input,
            };
            current_agent = target;
        }
        Err("maximum handoff depth exceeded".to_string())
    }

    fn run_single_agent(
        &self,
        agent: &Agent,
        input: NormalizedInput,
        config: RunConfig,
        event_collector: Option<Arc<std::sync::Mutex<Vec<RunEvent>>>>,
    ) -> Result<SingleRunOutcome, String> {
        let provider = config
            .model_provider
            .clone()
            .or_else(|| self.default_run_config.model_provider.clone())
            .unwrap_or_else(|| self.model_provider.clone());
        let model_ref = config
            .model
            .clone()
            .or_else(|| self.default_run_config.model.clone())
            .or_else(|| agent.model().cloned())
            .ok_or_else(|| "agent model is not configured".to_string())?;
        let resolved = provider.resolve(&model_ref).map_err(format_model_error)?;
        let llm = provider.client(&resolved).map_err(format_model_error)?;
        let provider_settings = provider.default_settings(&resolved);
        let settings = provider_settings
            .merge(agent.model_settings())
            .merge(
                self.default_run_config
                    .model_settings
                    .as_ref()
                    .unwrap_or(&crate::model_settings::ModelSettings::default()),
            )
            .merge(
                config
                    .model_settings
                    .as_ref()
                    .unwrap_or(&crate::model_settings::ModelSettings::default()),
            );
        let workspace = config
            .workspace
            .clone()
            .or_else(|| self.default_run_config.workspace.clone())
            .unwrap_or_else(|| self.workspace.clone());
        let session = config
            .session
            .clone()
            .or_else(|| self.default_run_config.session.clone());
        let session_items = if let Some(session) = session.as_ref() {
            block_on_session(session.get_items(None))?
        } else {
            Vec::new()
        };
        let tool_policy = merged_tool_policy(
            agent.tool_policy(),
            &self.default_run_config.tool_policy,
            &config.tool_policy,
        );
        let run_context = RunContext {
            run_id: format!("{}_run", agent.name()),
            agent_name: agent.name().to_string(),
            model: Some(model_ref.clone()),
            workspace: Some(workspace.clone()),
            metadata: config.metadata.clone(),
        };
        let guarded_input = apply_input_guardrails(agent, &run_context, input)?;
        let input_text = guarded_input.text;
        let mut task = AgentTask::new(
            format!("{}_run", agent.name()),
            resolved.model_id.clone(),
            agent.instructions().to_string(),
            input_text.clone(),
        );
        task.max_cycles = config
            .max_cycles
            .or(self.default_run_config.max_cycles)
            .or(agent.max_cycles())
            .unwrap_or(10)
            .max(1);
        task.no_tool_policy = NoToolPolicy::Continue;
        task.metadata = agent.metadata().clone();
        task.metadata
            .extend(self.default_run_config.metadata.clone());
        task.metadata.extend(config.metadata.clone());
        task.metadata
            .entry("agent_name".to_string())
            .or_insert_with(|| Value::String(agent.name().to_string()));
        task.metadata
            .insert("model_settings".to_string(), settings.to_value());
        task.metadata.insert(
            "runtime_model".to_string(),
            serde_json::to_value(&resolved).unwrap_or(Value::Null),
        );
        apply_resolved_model_limits(&mut task, &resolved);
        task.initial_messages = session_items
            .iter()
            .map(SessionItem::to_message)
            .collect::<Vec<_>>();
        let mut registry = self.tool_registry.clone();
        for handoff in agent.handoffs() {
            registry.register(handoff.as_tool_spec(agent.name()))?;
        }
        for tool in agent.tools() {
            registry.register(tool.as_tool_spec())?;
        }
        let mut runtime = AgentRuntime::new(ArcLlmClient(llm))
            .with_tool_registry(registry)
            .with_settings_file("__runner_model_provider__")
            .with_default_backend(resolved.backend.clone());
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
        if approval_required(&tool_policy) {
            runtime.hooks.push(Arc::new(ApprovalHook::new(
                tool_policy.clone(),
                task.metadata.clone(),
            )));
        }
        let log_handler = event_collector.as_ref().map(|collector| {
            let collector = collector.clone();
            Arc::new(
                move |event: &str, payload: &std::collections::BTreeMap<String, Value>| {
                    if let Some(mapped) = map_runtime_event(event, payload) {
                        if let Ok(mut events) = collector.lock() {
                            events.push(mapped);
                        }
                    }
                },
            ) as crate::runtime::RuntimeEventHandler
        });
        let controls = RuntimeRunControls {
            log_handler,
            cancellation_token: config
                .cancellation_token
                .clone()
                .or_else(|| self.default_run_config.cancellation_token.clone()),
            execution_context: Some(ExecutionContext {
                metadata: task.metadata.clone(),
                ..ExecutionContext::default()
            }),
            workspace: Some(workspace),
            workspace_backend: runtime.workspace_backend.clone().into(),
            model_provider: Some(provider.clone()),
            ..RuntimeRunControls::default()
        };
        let trace_sink = config
            .trace_sink
            .clone()
            .or_else(|| self.default_run_config.trace_sink.clone());
        let run_span = Span::new(
            format!("{}_run", agent.name()),
            "run",
            Some(agent.name().to_string()),
        );
        let agent_span = Span::new(
            format!("{}_run", agent.name()),
            "agent",
            Some(agent.name().to_string()),
        );
        if let Some(trace_sink) = trace_sink.as_ref() {
            trace_sink.on_span_start(&run_span);
            trace_sink.on_span_start(&agent_span);
        }
        let result = runtime
            .run_with_controls(task, controls)
            .map_err(|error| error.to_string())?;
        if let Some(trace_sink) = trace_sink.as_ref() {
            trace_sink.on_span_end(&agent_span);
            trace_sink.on_span_end(&run_span);
            trace_sink.flush()?;
        }
        let result = apply_output_guardrails(agent, &run_context, result);
        let handoff = extract_handoff(&result);
        if let Some(session) = session.as_ref() {
            let mut new_items = Vec::new();
            new_items.push(SessionItem::User {
                content: input_text.clone(),
            });
            if handoff.is_none() {
                if let Some(answer) = result.final_answer.as_ref() {
                    new_items.push(SessionItem::Assistant {
                        content: answer.clone(),
                    });
                }
            } else if let Some(handoff) = handoff.as_ref() {
                new_items.push(SessionItem::Assistant {
                    content: format!(
                        "Handed off from {} to {}.",
                        handoff.from_agent, handoff.to_agent
                    ),
                });
                if !handoff.input.is_empty() {
                    new_items.push(SessionItem::User {
                        content: handoff.input.clone(),
                    });
                }
            }
            block_on_session(session.add_items(new_items))?;
        }
        Ok(SingleRunOutcome {
            result: RunResult::new(agent.name().to_string(), result, resolved).with_resume_context(
                RunResumeContext {
                    agent: agent.clone(),
                    input: NormalizedInput {
                        text: input_text.clone(),
                    },
                    config,
                },
            ),
            handoff,
        })
    }

    pub async fn stream(
        &self,
        agent: &Agent,
        input: impl Into<NormalizedInput>,
    ) -> Result<RunEventStream, String> {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let result = self.run_blocking(
            agent,
            input.into(),
            RunConfig::default(),
            Some(events.clone()),
        )?;
        let collected = events
            .lock()
            .map_err(|_| "stream event lock poisoned".to_string())?
            .clone();
        Ok(RunEventStream::from_events(result, collected))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HandoffRequest {
    from_agent: String,
    to_agent: String,
    input: String,
}

struct SingleRunOutcome {
    result: RunResult,
    handoff: Option<HandoffRequest>,
}

struct ApprovedToolCall {
    call: crate::types::ToolCall,
    cycle_index: u32,
}

fn apply_input_guardrails(
    agent: &Agent,
    context: &RunContext,
    input: NormalizedInput,
) -> Result<NormalizedInput, String> {
    let mut current = input;
    for guardrail in agent.input_guardrails() {
        current = match guardrail.check(context, &current) {
            GuardrailOutcome::Allow(input) => input,
            GuardrailOutcome::Block { message } => return Err(message),
            GuardrailOutcome::RequireApproval { message } => return Err(message),
        };
    }
    Ok(current)
}

fn apply_output_guardrails(
    agent: &Agent,
    context: &RunContext,
    result: AgentResult,
) -> AgentResult {
    let mut current = result;
    for guardrail in agent.output_guardrails() {
        current = match guardrail.check(context, &current) {
            GuardrailOutcome::Allow(output) => output,
            GuardrailOutcome::Block { message } | GuardrailOutcome::RequireApproval { message } => {
                let mut failed = current.clone();
                failed.status = crate::types::AgentStatus::Failed;
                failed.error = Some(message);
                failed.final_answer = None;
                failed
            }
        };
    }
    current
}

fn max_handoff_depth(config: &RunConfig, agent: &Agent) -> u32 {
    config
        .max_cycles
        .or(agent.max_cycles())
        .unwrap_or(10)
        .max(1)
}

fn push_event(collector: Option<&Arc<std::sync::Mutex<Vec<RunEvent>>>>, event: RunEvent) {
    if let Some(collector) = collector {
        if let Ok(mut events) = collector.lock() {
            events.push(event);
        }
    }
}

fn find_approved_tool_call(
    result: &AgentResult,
    approved_ids: &[String],
) -> Option<ApprovedToolCall> {
    for cycle in &result.cycles {
        for tool_result in &cycle.tool_results {
            let interruption_id = tool_result
                .metadata
                .get("approval_interruption_id")
                .and_then(Value::as_str)?;
            if !approved_ids.iter().any(|id| id == interruption_id) {
                continue;
            }
            let tool_name = tool_result
                .metadata
                .get("tool_name")
                .and_then(Value::as_str)?;
            let call = cycle
                .tool_calls
                .iter()
                .find(|call| call.id == tool_result.tool_call_id && call.name == tool_name)
                .cloned()
                .or_else(|| {
                    cycle
                        .tool_calls
                        .iter()
                        .find(|call| call.name == tool_name)
                        .cloned()
                })?;
            return Some(ApprovedToolCall {
                call,
                cycle_index: cycle.index,
            });
        }
    }
    None
}

fn extract_handoff(result: &AgentResult) -> Option<HandoffRequest> {
    result
        .cycles
        .iter()
        .flat_map(|cycle| cycle.tool_results.iter())
        .find_map(|tool_result| {
            let is_handoff = tool_result
                .metadata
                .get("handoff")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !is_handoff {
                return None;
            }
            let from_agent = tool_result
                .metadata
                .get("from_agent")
                .and_then(Value::as_str)?
                .to_string();
            let to_agent = tool_result
                .metadata
                .get("to_agent")
                .and_then(Value::as_str)?
                .to_string();
            let input = tool_result
                .metadata
                .get("handoff_input")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            Some(HandoffRequest {
                from_agent,
                to_agent,
                input,
            })
        })
}

fn approval_required(policy: &ToolPolicy) -> bool {
    !matches!(policy.approval, ApprovalPolicy::Never)
}

fn merged_tool_policy(agent: &ToolPolicy, runner: &ToolPolicy, run: &ToolPolicy) -> ToolPolicy {
    let mut merged = agent.clone();
    if runner.allowed_tools.is_some() {
        merged.allowed_tools = runner.allowed_tools.clone();
    }
    if run.allowed_tools.is_some() {
        merged.allowed_tools = run.allowed_tools.clone();
    }
    merged
        .disallowed_tools
        .extend(runner.disallowed_tools.clone());
    merged.disallowed_tools.extend(run.disallowed_tools.clone());
    merged.approval = match run.approval {
        ApprovalPolicy::Never if !matches!(runner.approval, ApprovalPolicy::Never) => {
            runner.approval.clone()
        }
        ApprovalPolicy::Never if !matches!(agent.approval, ApprovalPolicy::Never) => {
            agent.approval.clone()
        }
        _ => run.approval.clone(),
    };
    if let Some(max_concurrency) = runner.max_concurrency {
        merged.max_concurrency = Some(max_concurrency);
    }
    if let Some(max_concurrency) = run.max_concurrency {
        merged.max_concurrency = Some(max_concurrency);
    }
    merged
}

struct ApprovalHook {
    policy: ToolPolicy,
    approved_ids: Vec<String>,
}

impl ApprovalHook {
    fn new(policy: ToolPolicy, metadata: crate::types::Metadata) -> Self {
        let approved_ids = metadata
            .get("approved_tool_interruption_ids")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            policy,
            approved_ids,
        }
    }
}

impl RuntimeHook for ApprovalHook {
    fn before_tool_call(&self, event: BeforeToolCallEvent<'_>) -> Option<BeforeToolCallPatch> {
        if let Some(allowed) = self.policy.allowed_tools.as_ref() {
            if !allowed.iter().any(|tool| tool == &event.call.name) {
                return Some(BeforeToolCallPatch::result(approval_error(
                    &event.call.id,
                    &event.call.name,
                    "tool_not_allowed",
                    "Tool is not in the allowed tool list.",
                )));
            }
        }
        if self
            .policy
            .disallowed_tools
            .iter()
            .any(|tool| tool == &event.call.name)
        {
            return Some(BeforeToolCallPatch::result(approval_error(
                &event.call.id,
                &event.call.name,
                "tool_disallowed",
                "Tool is disallowed by policy.",
            )));
        }
        if !matches!(self.policy.approval, ApprovalPolicy::Always) {
            return None;
        }
        let interruption_id = approval_interruption_id(event.task.task_id.as_str(), event.call);
        if self
            .approved_ids
            .iter()
            .any(|approved| approved == &interruption_id)
        {
            return None;
        }
        Some(BeforeToolCallPatch::result(approval_required_result(
            &event.call.id,
            &event.call.name,
            &interruption_id,
        )))
    }
}

fn approval_interruption_id(task_id: &str, call: &crate::types::ToolCall) -> String {
    format!("approval:{task_id}:{}:{}", call.name, call.id)
}

fn approval_required_result(
    tool_call_id: &str,
    tool_name: &str,
    interruption_id: &str,
) -> ToolExecutionResult {
    let mut metadata = crate::types::Metadata::new();
    metadata.insert("approval_required".to_string(), Value::Bool(true));
    metadata.insert(
        "approval_interruption_id".to_string(),
        Value::String(interruption_id.to_string()),
    );
    metadata.insert(
        "tool_name".to_string(),
        Value::String(tool_name.to_string()),
    );
    ToolExecutionResult {
        tool_call_id: tool_call_id.to_string(),
        content: json!({
            "ok": false,
            "approval_required": true,
            "interruption_id": interruption_id,
            "tool_name": tool_name,
        })
        .to_string(),
        status: ToolResultStatus::WaitResponse,
        directive: ToolDirective::WaitUser,
        error_code: Some("tool_approval_required".to_string()),
        metadata,
        image_url: None,
        image_path: None,
    }
}

fn approval_error(
    tool_call_id: &str,
    tool_name: &str,
    error_code: &str,
    message: &str,
) -> ToolExecutionResult {
    ToolExecutionResult {
        tool_call_id: tool_call_id.to_string(),
        content: json!({
            "ok": false,
            "error": message,
            "error_code": error_code,
            "tool_name": tool_name,
        })
        .to_string(),
        status: ToolResultStatus::Error,
        directive: ToolDirective::Continue,
        error_code: Some(error_code.to_string()),
        metadata: crate::types::Metadata::new(),
        image_url: None,
        image_path: None,
    }
}

fn completed_from_first_tool_result(result: RunResult) -> RunResult {
    if result.status() != crate::types::AgentStatus::MaxCycles {
        return result;
    }
    let Some(tool_result) = result
        .result()
        .cycles
        .iter()
        .flat_map(|cycle| cycle.tool_results.iter())
        .find(|tool_result| tool_result.status == ToolResultStatus::Success)
        .cloned()
    else {
        return result;
    };
    let final_answer = tool_result.content.clone();
    let agent_name = result.agent_name().to_string();
    let resolved = result.resolved_model().clone();
    let mut agent_result = result.result().clone();
    agent_result.status = crate::types::AgentStatus::Completed;
    agent_result.final_answer = Some(final_answer);
    RunResult::new(agent_name, agent_result, resolved)
}

#[derive(Default)]
pub struct RunnerBuilder {
    model_provider: Option<Arc<dyn ModelProvider>>,
    settings_file: Option<PathBuf>,
    default_backend: Option<String>,
    workspace: Option<PathBuf>,
    tool_registry: Option<ToolRegistry>,
    default_run_config: RunConfig,
}

impl RunnerBuilder {
    pub fn model_provider(mut self, provider: impl ModelProvider + 'static) -> Self {
        self.model_provider = Some(Arc::new(provider));
        self
    }

    pub fn model_provider_arc(mut self, provider: Arc<dyn ModelProvider>) -> Self {
        self.model_provider = Some(provider);
        self
    }

    pub fn settings_file(mut self, settings_file: impl Into<PathBuf>) -> Self {
        self.settings_file = Some(settings_file.into());
        self
    }

    pub fn default_backend(mut self, default_backend: impl Into<String>) -> Self {
        self.default_backend = Some(default_backend.into());
        self
    }

    pub fn workspace(mut self, workspace: impl Into<PathBuf>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    pub fn tool_registry(mut self, registry: ToolRegistry) -> Self {
        self.tool_registry = Some(registry);
        self
    }

    pub fn default_run_config(mut self, config: RunConfig) -> Self {
        self.default_run_config = config;
        self
    }

    pub fn build(self) -> Result<Runner, String> {
        let model_provider = if let Some(provider) = self.model_provider {
            provider
        } else {
            let settings_file = self
                .settings_file
                .unwrap_or_else(|| PathBuf::from("local_settings.json"));
            let mut provider = VvLlmModelProvider::from_settings_file(settings_file);
            if let Some(default_backend) = self.default_backend {
                provider = provider.with_default_backend(default_backend);
            }
            Arc::new(provider)
        };
        Ok(Runner {
            model_provider,
            workspace: self
                .workspace
                .unwrap_or_else(|| PathBuf::from("./workspace")),
            tool_registry: self
                .tool_registry
                .unwrap_or_else(crate::tools::build_default_registry),
            default_run_config: self.default_run_config,
        })
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
