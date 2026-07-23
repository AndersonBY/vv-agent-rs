use super::*;

pub(super) fn prepare_checkpoint_resume(
    agent: &Agent,
    session: Option<&Arc<dyn crate::sessions::Session>>,
    config: Option<&CheckpointConfig>,
) -> Result<(Option<Checkpoint>, bool), String> {
    if config.is_some() && !agent.handoffs().is_empty() {
        return Err(
            "checkpoint_handoff_unsupported: checkpoint v2 does not yet support handoff state"
                .to_string(),
        );
    }
    if config.is_some() && session.is_some_and(|session| !session.supports_add_items_once()) {
        return Err(
            "checkpoint_session_idempotency_unsupported: checkpoint v2 requires an append-once session"
                .to_string(),
        );
    }
    let checkpoint = preload_checkpoint(config)?;
    let resume = checkpoint.is_some()
        && config.is_some_and(|config| config.resume_policy != ResumePolicy::New);
    Ok((checkpoint, resume))
}

pub(super) struct CheckpointRuntimeRequest<'a> {
    pub config: Option<CheckpointConfig>,
    pub agent: &'a Agent,
    pub input_text: &'a str,
    pub run_config: &'a RunConfig,
    pub resolved: &'a crate::config::ResolvedModelConfig,
    pub model_settings: &'a crate::model_settings::ModelSettings,
    pub task: &'a AgentTask,
    pub registry: &'a ToolRegistry,
    pub definition_initial_messages: &'a [crate::types::Message],
    pub run_id: &'a str,
    pub trace_id: &'a str,
    pub initial_budget_usage: Option<crate::budget::BudgetUsageSnapshot>,
    pub extensions: Vec<Arc<dyn crate::checkpoint::CheckpointExtension>>,
    pub reconciliation_provider: Option<Arc<dyn crate::checkpoint::ReconciliationProvider>>,
    pub event_collector: Option<Arc<Mutex<Vec<RunEvent>>>>,
    pub event_sender: Option<broadcast::Sender<RunEvent>>,
    pub event_store: Option<Arc<dyn crate::event_store::RunEventStore>>,
    pub preloaded_checkpoint: Option<Checkpoint>,
    pub checkpoint_resume: bool,
    pub backend_manages_checkpoint_cycles: bool,
    pub admission_sender: &'a mut Option<CheckpointAdmissionSender>,
}

pub(super) struct CheckpointRuntimeState {
    pub controller: Option<CheckpointController>,
    pub terminal_replayed: bool,
    pub replayed_result: Option<AgentResult>,
    pub initial_budget_usage: Option<crate::budget::BudgetUsageSnapshot>,
    pub initial_messages: Option<Vec<crate::types::Message>>,
    pub initial_cycles: Option<Vec<crate::types::CycleRecord>>,
    pub initial_model_calls: Option<Vec<crate::types::ModelCallRecord>>,
    pub initial_shared_state: Option<crate::types::Metadata>,
    pub cycle_index_start: Option<u32>,
    pub cycle_count: Option<u32>,
}

pub(super) fn prepare_checkpoint_runtime(
    request: CheckpointRuntimeRequest<'_>,
) -> Result<CheckpointRuntimeState, String> {
    let Some(config) = request.config else {
        return Ok(CheckpointRuntimeState {
            controller: None,
            terminal_replayed: false,
            replayed_result: None,
            initial_budget_usage: request.initial_budget_usage,
            initial_messages: None,
            initial_cycles: None,
            initial_model_calls: None,
            initial_shared_state: None,
            cycle_index_start: None,
            cycle_count: None,
        });
    };

    let (run_definition, run_definition_digest) = build_run_definition(RunDefinitionRequest {
        agent: request.agent,
        root_input: request.input_text,
        run_config: request.run_config,
        resolved: request.resolved,
        model_settings: request.model_settings,
        task: request.task,
        registry: request.registry,
        initial_messages: request.definition_initial_messages,
    })
    .map_err(|error| error.to_string())?;
    let prepared_initial_messages = request
        .preloaded_checkpoint
        .as_ref()
        .filter(|_| request.checkpoint_resume)
        .map(|checkpoint| checkpoint.messages.clone())
        .unwrap_or_else(|| crate::runtime::engine::build_initial_messages(request.task));
    let checkpoint_event_sink: CheckpointEventSink = {
        let collector = request.event_collector;
        let event_sender = request.event_sender;
        Arc::new(move |event| {
            capture_event(
                collector.as_ref(),
                event_sender.as_ref(),
                None,
                false,
                event,
            )
        })
    };
    let mut controller = CheckpointResumeController::new(CheckpointControllerRequest {
        config,
        task_id: request.task.task_id.clone(),
        run_id: request.run_id.to_string(),
        trace_id: request.trace_id.to_string(),
        agent_name: request.agent.name().to_string(),
        run_definition,
        run_definition_digest,
        initial_messages: prepared_initial_messages,
        initial_shared_state: request.task.initial_shared_state.clone(),
        initial_budget_usage: request.initial_budget_usage.clone(),
        extensions: request.extensions,
        reconciliation_provider: request.reconciliation_provider,
        event_sink: checkpoint_event_sink,
        event_store: request.event_store,
        preloaded_checkpoint: request.preloaded_checkpoint,
    })
    .map_err(|error| error.to_string())?;
    let mut replayed_result = controller.admit().map_err(|error| error.to_string())?;
    let terminal_replayed = replayed_result.is_some();
    let completed_cycles = u32::try_from(
        controller
            .checkpoint()
            .map_err(|error| error.to_string())?
            .cycle_index,
    )
    .map_err(|_| "checkpoint_cycle_invalid: checkpoint cycle_index exceeds u32".to_string())?;
    let cycle_index_start = completed_cycles.saturating_add(1);
    if replayed_result.is_none() && !request.backend_manages_checkpoint_cycles {
        replayed_result = controller
            .begin_cycle(cycle_index_start)
            .map_err(|error| error.to_string())?;
    }
    let checkpoint = controller
        .checkpoint()
        .map_err(|error| error.to_string())?
        .clone();
    if let Some(sender) = request.admission_sender.take() {
        let _ = sender.send(CheckpointAdmission {
            checkpoint: checkpoint.clone(),
            terminal_replayed,
        });
    }

    Ok(CheckpointRuntimeState {
        controller: Some(Arc::new(Mutex::new(controller))),
        terminal_replayed,
        replayed_result,
        initial_budget_usage: checkpoint.budget_usage.clone(),
        initial_messages: Some(checkpoint.messages.clone()),
        initial_cycles: Some(checkpoint.cycles.clone()),
        initial_model_calls: Some(checkpoint.model_calls.clone()),
        initial_shared_state: Some(checkpoint.shared_state.clone()),
        cycle_index_start: Some(cycle_index_start),
        cycle_count: Some(request.task.max_cycles.saturating_sub(completed_cycles)),
    })
}

pub(super) fn replay_checkpoint_terminal(
    controller: Option<&CheckpointController>,
    mut terminal_replayed: bool,
    mut result: AgentResult,
) -> Result<(AgentResult, bool), String> {
    if !terminal_replayed {
        if let Some(controller) = controller {
            if let Some(replayed) = controller
                .lock()
                .map_err(|_| checkpoint_lock_error())?
                .replay_terminal_if_present()
                .map_err(|error| error.to_string())?
            {
                result = replayed;
                terminal_replayed = true;
            }
        }
    }
    Ok((result, terminal_replayed))
}

pub(super) fn prepare_checkpoint_terminal(
    controller: Option<&CheckpointController>,
    terminal_replayed: bool,
    result: AgentResult,
) -> Result<AgentResult, String> {
    let Some(controller) = controller.filter(|_| !terminal_replayed) else {
        return Ok(result);
    };
    controller
        .lock()
        .map_err(|_| checkpoint_lock_error())?
        .prepare_terminal(result)
        .map_err(|error| error.to_string())
}

fn checkpoint_lock_error() -> String {
    "checkpoint_store_lock_poisoned: checkpoint controller lock poisoned".to_string()
}
