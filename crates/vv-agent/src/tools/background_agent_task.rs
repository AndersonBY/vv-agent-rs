use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::agent::Agent;
use crate::result::RunResult;
use crate::run_config::RunConfig;
use crate::runner::{NormalizedInput, Runner};
use crate::runtime::CancellationToken;
use crate::tools::{Tool, ToolContext, ToolOutput, ToolSpec, ToolSpecKind};
use crate::types::{AgentStatus, ToolArguments};

static NEXT_BACKGROUND_AGENT_TASK_ID: AtomicU64 = AtomicU64::new(1);
const BACKGROUND_AGENT_TASK_WORKER_PANIC: &str = "background agent task worker panicked";

#[derive(Clone)]
pub struct BackgroundAgentTask {
    agent: Agent,
    name: String,
    description: String,
    parameters_schema: Value,
    handles: Arc<Mutex<BTreeMap<String, BackgroundAgentTaskHandle>>>,
}

impl BackgroundAgentTask {
    pub fn start(
        &self,
        runner: &Runner,
        context: &ToolContext,
        raw_arguments: Value,
        run_config: Option<RunConfig>,
    ) -> Result<BackgroundAgentTaskHandle, String> {
        let input = self.input_from_arguments(raw_arguments)?;
        let run_config = inherited_run_config(context, run_config);
        let task_id = format!(
            "bg_agent_{:012x}",
            NEXT_BACKGROUND_AGENT_TASK_ID.fetch_add(1, Ordering::Relaxed)
        );
        let state = Arc::new(Mutex::new(BackgroundAgentTaskState {
            status: AgentStatus::Running,
            result: None,
            error: None,
        }));
        let state_for_worker = state.clone();
        let runner = runner.clone();
        let agent = self.agent.clone();
        let task_id_for_error = task_id.clone();
        let _ = std::thread::Builder::new()
            .name(format!("vv-agent-background-{task_id}"))
            .spawn(move || {
                let update = catch_unwind(AssertUnwindSafe(|| {
                    match runner.run_blocking(
                        &agent,
                        NormalizedInput::from(input),
                        run_config,
                        None,
                    ) {
                        Ok(result) => {
                            let status = result.status();
                            let error = result.result().error.clone();
                            (status, Some(result), error)
                        }
                        Err(error) => (AgentStatus::Failed, None, Some(error)),
                    }
                }))
                .unwrap_or_else(|_| {
                    (
                        AgentStatus::Failed,
                        None,
                        Some(BACKGROUND_AGENT_TASK_WORKER_PANIC.to_string()),
                    )
                });
                if let Ok(mut state) = state_for_worker.lock() {
                    (state.status, state.result, state.error) = update;
                }
            })
            .map_err(|error| {
                if let Ok(mut state) = state.lock() {
                    state.status = AgentStatus::Failed;
                    state.error = Some(error.to_string());
                }
                format!("failed to spawn background agent task {task_id_for_error}: {error}")
            })?;
        let handle = BackgroundAgentTaskHandle {
            task_id,
            agent_name: self.agent.name().to_string(),
            state,
        };
        self.handles
            .lock()
            .map_err(|_| "background task registry lock poisoned".to_string())?
            .insert(handle.task_id.clone(), handle.clone());
        Ok(handle)
    }

    pub fn get_handle(&self, task_id: &str) -> Result<BackgroundAgentTaskHandle, String> {
        self.handles
            .lock()
            .map_err(|_| "background task registry lock poisoned".to_string())?
            .get(task_id)
            .cloned()
            .ok_or_else(|| format!("unknown background agent task: {task_id}"))
    }

    fn input_from_arguments(&self, raw_arguments: Value) -> Result<String, String> {
        let object = raw_arguments
            .as_object()
            .ok_or_else(|| "background task arguments must be an object".to_string())?;
        object
            .get("task_description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| "background task requires task_description".to_string())
    }
}

impl Tool for BackgroundAgentTask {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> &Value {
        &self.parameters_schema
    }

    fn as_tool_spec(&self) -> ToolSpec {
        let name = self.name.clone();
        let description = self.description.clone();
        let parameters_schema = self.parameters_schema.clone();
        let task = self.clone();
        let mut spec = ToolSpec::new(
            name.clone(),
            description.clone(),
            Arc::new(
                move |context: &mut ToolContext, arguments: &ToolArguments| {
                    let raw_arguments = Value::Object(arguments.clone().into_iter().collect());
                    let task_description = match task.input_from_arguments(raw_arguments.clone()) {
                        Ok(task_description) => task_description,
                        Err(error) => {
                            return ToolOutput::error(error)
                                .with_code("invalid_background_task_arguments")
                                .to_result(&context.tool_call_id)
                        }
                    };
                    let Some(model_provider) = context.model_provider.clone() else {
                        return ToolOutput::error("background agent runtime is not available")
                            .with_code("background_agent_runtime_unavailable")
                            .to_result(&context.tool_call_id);
                    };
                    let runner = match Runner::builder()
                        .model_provider_arc(model_provider)
                        .workspace(context.workspace.clone())
                        .build()
                    {
                        Ok(runner) => runner,
                        Err(error) => {
                            return ToolOutput::error(error)
                                .with_code("background_agent_runtime_unavailable")
                                .to_result(&context.tool_call_id)
                        }
                    };
                    match task.start(&runner, context, raw_arguments, None) {
                        Ok(handle) => ToolOutput::json(json!({
                            "agent_name": task.agent.name(),
                            "status": "background_task_started",
                            "task_description": task_description,
                            "task_id": handle.task_id(),
                        }))
                        .to_result(&context.tool_call_id),
                        Err(error) => ToolOutput::error(error)
                            .with_code("background_agent_start_failed")
                            .to_result(&context.tool_call_id),
                    }
                },
            ),
        );
        spec.kind = ToolSpecKind::BackgroundAgent;
        spec.schema = json!({
            "type": "function",
            "function": {
                "name": name,
                "description": description,
                "parameters": parameters_schema,
            }
        });
        spec
    }
}

pub struct BackgroundAgentTaskBuilder {
    agent: Agent,
    name: Option<String>,
    description: Option<String>,
}

impl BackgroundAgentTaskBuilder {
    pub fn new(agent: Agent) -> Self {
        Self {
            agent,
            name: None,
            description: None,
        }
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn build(self) -> Result<BackgroundAgentTask, String> {
        let name = self
            .name
            .unwrap_or_else(|| format!("{}_background_task", self.agent.name()));
        if name.trim().is_empty() {
            return Err("background task tool name cannot be empty".to_string());
        }
        let description = self.description.unwrap_or_else(|| {
            format!(
                "Start the {} agent as a background task.",
                self.agent.name()
            )
        });
        Ok(BackgroundAgentTask {
            agent: self.agent,
            name,
            description,
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "task_description": {
                        "type": "string",
                        "description": "Task for the background agent."
                    }
                },
                "required": ["task_description"],
                "additionalProperties": false
            }),
            handles: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }
}

pub(crate) fn inherited_run_config(
    context: &ToolContext,
    run_config: Option<RunConfig>,
) -> RunConfig {
    let explicit_shared_state = run_config
        .as_ref()
        .map(|config| config.initial_shared_state.clone())
        .unwrap_or_default();
    let explicit_metadata = run_config
        .as_ref()
        .map(|config| config.metadata.clone())
        .unwrap_or_default();
    let has_projected_parent = context.background_parent_run_config.is_some();
    let mut config = context
        .background_parent_run_config
        .clone()
        .or(run_config)
        .unwrap_or_default();
    if config.workspace.is_none() {
        config.workspace = Some(context.workspace.clone());
    }
    if config.workspace_backend.is_none() {
        config.workspace_backend = Some(context.workspace_backend.clone());
    }
    if config.model_provider.is_none() {
        config.model_provider = context.model_provider.clone();
    }
    if config.execution_backend.is_none() {
        config.execution_backend = context.execution_backend.clone();
    }
    if config.app_state.is_none() {
        config.app_state = context.app_state.clone();
    }
    if config.cancellation_token.is_none() {
        config.cancellation_token = context
            .sub_task_turn_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.cancellation_token.as_ref())
            .cloned();
    }
    let scoped_child_cancellation = config
        .cancellation_token
        .is_none()
        .then(CancellationToken::child_of_current)
        .flatten();

    let mut shared_state = context.shared_state.clone();
    shared_state.extend(explicit_shared_state);

    if !has_projected_parent {
        config.metadata = context.metadata.clone();
        for key in [
            "agent_name",
            "session_id",
            "approved_tool_interruption_ids",
            "_vv_agent_run_id",
            "_vv_agent_trace_id",
            "_vv_agent_agent_name",
            "_vv_agent_input",
            "_vv_agent_session_id",
            "_vv_agent_tool_use_behavior",
            "_vv_agent_stop_at_tool_names",
        ] {
            config.metadata.remove(key);
        }
    }
    config.metadata.extend(explicit_metadata);
    let mut child = config.for_background_child(shared_state);
    if child.cancellation_token.is_none() {
        child.cancellation_token = scoped_child_cancellation;
    }
    child
}

#[derive(Clone)]
pub struct BackgroundAgentTaskHandle {
    task_id: String,
    agent_name: String,
    state: Arc<Mutex<BackgroundAgentTaskState>>,
}

impl BackgroundAgentTaskHandle {
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    pub fn agent_name(&self) -> &str {
        &self.agent_name
    }

    pub fn status(&self) -> AgentStatus {
        self.state
            .lock()
            .map(|state| state.status)
            .unwrap_or(AgentStatus::Failed)
    }

    pub fn poll(&self) -> Result<BackgroundAgentTaskSnapshot, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "background task lock poisoned".to_string())?;
        Ok(BackgroundAgentTaskSnapshot {
            task_id: self.task_id.clone(),
            agent_name: self.agent_name.clone(),
            status: state.status,
            final_output: state
                .result
                .as_ref()
                .and_then(|result| result.final_output().map(str::to_string)),
            error: state.error.clone(),
        })
    }

    pub async fn wait(&self) -> Result<BackgroundAgentTaskSnapshot, String> {
        loop {
            let snapshot = self.poll()?;
            if !matches!(snapshot.status, AgentStatus::Running | AgentStatus::Pending) {
                return Ok(snapshot);
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    pub async fn wait_with_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> Result<BackgroundAgentTaskSnapshot, String> {
        tokio::time::timeout(timeout, self.wait())
            .await
            .map_err(|_| {
                format!(
                    "background agent task {} was not ready before timeout",
                    self.task_id
                )
            })?
    }
}

struct BackgroundAgentTaskState {
    status: AgentStatus,
    result: Option<RunResult>,
    error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundAgentTaskSnapshot {
    task_id: String,
    agent_name: String,
    status: AgentStatus,
    final_output: Option<String>,
    error: Option<String>,
}

impl BackgroundAgentTaskSnapshot {
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    pub fn agent_name(&self) -> &str {
        &self.agent_name
    }

    pub fn status(&self) -> AgentStatus {
        self.status
    }

    pub fn final_output(&self) -> Option<&str> {
        self.final_output.as_deref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::inherited_run_config;
    use crate::context_providers::{
        ContextError, ContextFragment, ContextProvider, ContextRequest,
    };
    use crate::memory::{
        MemoryFuture, MemoryProvider, MemorySaveRequest, MemorySaveResult, MemorySearchRequest,
        MemorySearchResult,
    };
    use crate::model::ModelRef;
    use crate::model_settings::ModelSettings;
    use crate::run_config::RunConfig;
    use crate::runtime::{CancellationToken, RuntimeHook, SubTaskManager, SubTaskTurnSnapshot};
    use crate::sessions::MemorySession;
    use crate::tools::{ApprovalPolicy, ToolContext, ToolPolicy, ToolRegistry};
    use crate::types::Message;

    struct NoopContextProvider;

    impl ContextProvider for NoopContextProvider {
        fn fragments(
            &self,
            _request: &ContextRequest<'_>,
        ) -> Result<Vec<ContextFragment>, ContextError> {
            Ok(Vec::new())
        }
    }

    struct NoopMemoryProvider;

    impl MemoryProvider for NoopMemoryProvider {
        fn search(&self, _request: MemorySearchRequest) -> MemoryFuture<Vec<MemorySearchResult>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn save(&self, _request: MemorySaveRequest) -> MemoryFuture<MemorySaveResult> {
            Box::pin(async { Ok(MemorySaveResult::default()) })
        }
    }

    struct NoopHook;

    impl RuntimeHook for NoopHook {}

    #[test]
    fn approval_resume_snapshot_derives_one_way_child_cancellation() {
        let snapshot_parent = CancellationToken::default();
        let fallback_parent = CancellationToken::default();
        let mut context = ToolContext::new("./workspace");
        context.sub_task_turn_snapshot = Some(SubTaskTurnSnapshot {
            cancellation_token: Some(snapshot_parent.clone()),
            ..SubTaskTurnSnapshot::default()
        });
        let child = {
            let _scope = CancellationToken::enter_scope(Some(&fallback_parent));
            inherited_run_config(&context, None)
                .cancellation_token
                .expect("derived child cancellation")
        };

        child.cancel();
        assert!(!snapshot_parent.is_cancelled());
        assert!(!fallback_parent.is_cancelled());

        let second_child = {
            let _scope = CancellationToken::enter_scope(Some(&fallback_parent));
            inherited_run_config(&context, None)
                .cancellation_token
                .expect("derived child cancellation")
        };
        fallback_parent.cancel();
        assert!(!second_child.is_cancelled());
        snapshot_parent.cancel();
        assert!(second_child.is_cancelled());
    }

    #[test]
    fn thread_local_and_explicit_cancellation_are_derived_for_the_child() {
        let context = ToolContext::new("./workspace");
        let fallback_parent = CancellationToken::default();
        let child = {
            let _scope = CancellationToken::enter_scope(Some(&fallback_parent));
            inherited_run_config(&context, None)
                .cancellation_token
                .expect("derived fallback child cancellation")
        };
        fallback_parent.cancel();
        assert!(child.is_cancelled());

        let unrelated_parent = CancellationToken::default();
        let explicit = CancellationToken::default();
        let config = {
            let _scope = CancellationToken::enter_scope(Some(&unrelated_parent));
            inherited_run_config(
                &context,
                Some(RunConfig {
                    cancellation_token: Some(explicit.clone()),
                    ..RunConfig::default()
                }),
            )
        };
        unrelated_parent.cancel();
        assert!(!explicit.is_cancelled());
        let explicit_child = config
            .cancellation_token
            .expect("derived explicit child cancellation");
        explicit_child.cancel();
        assert!(!explicit.is_cancelled());

        let second_explicit_child = inherited_run_config(
            &context,
            Some(RunConfig {
                cancellation_token: Some(explicit.clone()),
                ..RunConfig::default()
            }),
        )
        .cancellation_token
        .expect("second explicit child cancellation");
        explicit.cancel();
        assert!(second_explicit_child.is_cancelled());
        assert!(config.session.is_none());
        assert!(config.approval_broker.is_none());
    }

    #[test]
    fn projected_parent_preserves_capabilities_and_clears_run_instances() {
        let parent_cancellation = CancellationToken::default();
        let mut parent = RunConfig {
            model: Some(ModelRef::named("parent-model")),
            model_settings: Some(ModelSettings::default()),
            session: Some(Arc::new(MemorySession::new("parent-session"))),
            initial_messages: Some(vec![Message::user("parent history")]),
            max_cycles: Some(7),
            max_handoffs: Some(4),
            tool_policy: ToolPolicy {
                disallowed_tools: vec!["blocked".to_string()],
                approval: ApprovalPolicy::OnRequest,
                ..ToolPolicy::default()
            },
            cancellation_token: Some(parent_cancellation.clone()),
            hooks: vec![Arc::new(NoopHook)],
            context_providers: vec![Arc::new(NoopContextProvider)],
            max_context_chars: Some(12_345),
            memory_providers: vec![Arc::new(NoopMemoryProvider)],
            app_state: Some(Arc::new("parent app state".to_string())),
            tool_registry_factory: Some(Arc::new(ToolRegistry::default)),
            log_preview_chars: Some(321),
            before_cycle_messages: Some(Arc::new(|_, _, _| Vec::new())),
            interruption_messages: Some(Arc::new(Vec::new)),
            sub_task_manager: Some(SubTaskManager::default()),
            stream: Some(Arc::new(|_| {})),
            ..RunConfig::default()
        };
        parent
            .metadata
            .insert("parent_metadata".to_string(), json!("retained"));
        parent
            .initial_shared_state
            .insert("stale".to_string(), json!(true));

        let mut context = ToolContext::new("./workspace");
        context.background_parent_run_config = Some(parent);
        context
            .shared_state
            .insert("live".to_string(), json!("snapshot"));
        let child = inherited_run_config(&context, None);

        assert!(child.model.is_none());
        assert!(child.model_settings.is_none());
        assert!(child.session.is_none());
        assert!(child.initial_messages.is_none());
        assert!(child.before_cycle_messages.is_none());
        assert!(child.interruption_messages.is_none());
        assert!(child.sub_task_manager.is_none());
        assert!(child.stream.is_none());
        assert_eq!(
            child.initial_shared_state.get("live"),
            Some(&json!("snapshot"))
        );
        assert!(!child.initial_shared_state.contains_key("stale"));

        assert_eq!(child.max_cycles, Some(7));
        assert_eq!(child.max_handoffs, Some(4));
        assert_eq!(child.tool_policy.approval, ApprovalPolicy::OnRequest);
        assert_eq!(child.tool_policy.disallowed_tools, ["blocked"]);
        assert_eq!(child.hooks.len(), 1);
        assert_eq!(child.context_providers.len(), 1);
        assert_eq!(child.max_context_chars, Some(12_345));
        assert_eq!(child.memory_providers.len(), 1);
        assert!(child.app_state.is_some());
        assert!(child.tool_registry_factory.is_some());
        assert_eq!(child.log_preview_chars, Some(321));
        assert_eq!(child.metadata["parent_metadata"], json!("retained"));

        let child_cancellation = child
            .cancellation_token
            .expect("derived child cancellation");
        child_cancellation.cancel();
        assert!(!parent_cancellation.is_cancelled());
        let second_child = inherited_run_config(&context, None)
            .cancellation_token
            .expect("second child cancellation");
        parent_cancellation.cancel();
        assert!(second_child.is_cancelled());
    }
}
