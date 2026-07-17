//! Runtime-controller adaptation for native distributed execution.

use super::*;

pub(super) fn run_agent_runtime_cycle_v2(
    envelope: DistributedRunEnvelope,
    delivery: DistributedDeliveryMetadata,
    resolved: ResolvedDistributedCapabilities,
    store: Arc<dyn CheckpointStoreV2>,
    checkpoint: CheckpointV2,
) -> Result<CycleDispatchResult, String> {
    let config = envelope
        .checkpoint_config
        .as_ref()
        .expect("validated v2 envelope has checkpoint_config");
    let now_ms = now_unix_ms()?;
    let claim_mode = effective_claim_mode(&envelope, &checkpoint, delivery, now_ms);
    let event_store = checkpoint_event_store_adapter(&envelope, &resolved)?;
    let event_sink = checkpoint_event_sink(&resolved);
    let extensions = resolved
        .checkpoint_extensions
        .iter()
        .map(|extension| extension.extension.clone())
        .collect::<Vec<_>>();
    let agent_name = checkpoint
        .run_definition
        .pointer("/agent/name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("agent")
        .to_string();
    let runtime_config = CheckpointConfig {
        store: Some(store.clone()),
        store_ref: None,
        key: Some(config.key.clone()),
        resume_policy: crate::checkpoint::ResumePolicy::RequireExisting,
        ambiguous_model_policy: config.ambiguous_model_policy,
        ambiguous_tool_policy: config.ambiguous_tool_policy,
        required_extension_namespaces: config.required_extension_namespaces.clone(),
        max_extension_state_bytes: config.max_extension_state_bytes,
        credential_slots: config.credential_slots.clone(),
        capability_refs: Default::default(),
    };
    let mut controller = CheckpointResumeController::new(CheckpointControllerRequest {
        config: runtime_config,
        task_id: checkpoint.task_id.clone(),
        run_id: checkpoint.root_run_id.clone(),
        trace_id: checkpoint.trace_id.clone(),
        agent_name,
        run_definition: checkpoint.run_definition.clone(),
        run_definition_digest: checkpoint.run_definition_digest.clone(),
        initial_messages: checkpoint.messages.clone(),
        initial_shared_state: checkpoint.shared_state.clone(),
        initial_budget_usage: checkpoint.budget_usage.clone(),
        extensions,
        reconciliation_provider: resolved.reconciliation_provider.clone(),
        event_sink,
        event_store,
        preloaded_checkpoint: Some(checkpoint.clone()),
    })
    .map_err(|error| error.to_string())?;
    if let Some(replayed) = controller.admit().map_err(|error| error.to_string())? {
        let retained = controller
            .refresh_authoritative()
            .map_err(|error| error.to_string())?;
        controller.close();
        return Ok(CycleDispatchResult::terminal_replay(
            replayed,
            retained.revision,
        ));
    }
    controller
        .set_lease_duration_ms(envelope.lease_duration_ms)
        .map_err(|error| error.to_string())?;
    controller.set_next_claim_mode(claim_mode);
    let checkpoint_controller = Arc::new(Mutex::new(controller));
    let runtime = build_runtime(&envelope, &resolved)?;
    let mut task = envelope.task.clone();
    project_tool_policy(&mut task, &resolved.tool_policy);
    let execution_context = worker_execution_context(&envelope, &resolved);
    let previous_cycle_count = checkpoint.cycles.len();
    let controls = RuntimeRunControls {
        log_handler: combined_event_handler(&resolved),
        cancellation_token: resolved.cancellation.clone(),
        execution_context: Some(execution_context),
        workspace: Some(std::path::PathBuf::from(&envelope.recipe.workspace)),
        workspace_backend: resolved.workspace_backend.clone(),
        run_context: Some(RunContext {
            run_id: checkpoint.root_run_id.clone(),
            model: Some(ModelRef::backend(
                envelope.recipe.backend.clone(),
                envelope.recipe.model.clone(),
            )),
            workspace: Some(std::path::PathBuf::from(&envelope.recipe.workspace)),
            app_state: resolved.app_state.clone(),
            ..RunContext::default()
        }),
        sub_task_manager: resolved.sub_task_manager.clone(),
        budget_limits: envelope.budget_limits.clone(),
        host_cost_meter: resolved.host_cost_meter.clone(),
        initial_messages: Some(checkpoint.messages.clone()),
        initial_shared_state: Some(checkpoint.shared_state.clone()),
        initial_cycles: Some(checkpoint.cycles.clone()),
        cycle_index_start: Some(envelope.cycle_index),
        cycle_count: Some(1),
        initial_budget_usage: checkpoint.budget_usage.clone(),
        defer_terminal_on_max_cycles: true,
        checkpoint_controller: Some(CheckpointRuntimeControl::new(checkpoint_controller.clone())),
        ..RuntimeRunControls::default()
    };
    let result = runtime
        .run_with_controls(task, controls)
        .map_err(|error| error.to_string())?;
    let mut controller = checkpoint_controller
        .lock()
        .map_err(|_| "checkpoint controller lock poisoned".to_string())?;
    controller
        .assert_heartbeat_healthy()
        .map_err(|error| error.to_string())?;
    let current = controller
        .refresh_authoritative()
        .map_err(|error| error.to_string())?;

    if result.status == AgentStatus::MaxCycles
        && result.cycles.len() > previous_cycle_count
        && current.claim_token.is_none()
        && current.cycle_index == u64::from(envelope.cycle_index)
    {
        controller.close();
        return Ok(CycleDispatchResult::committed(
            current.cycle_index,
            current.revision,
        ));
    }
    if result.status == AgentStatus::ReconciliationRequired {
        if current.status != CheckpointStatus::ReconciliationRequired
            || current.claim_token.is_some()
        {
            return Err(
                "distributed reconciliation result does not match durable state".to_string(),
            );
        }
    } else {
        if current.terminal_result.is_some()
            || current.claim_token.is_none()
            || current.claimed_cycle != Some(u64::from(envelope.cycle_index))
        {
            return Err(
                "distributed terminal candidate does not retain the worker claim".to_string(),
            );
        }
    }
    let revision = controller
        .refresh_authoritative()
        .map_err(|error| error.to_string())?
        .revision;
    controller.close();
    Ok(CycleDispatchResult::terminal_candidate(result, revision))
}

pub(super) fn worker_execution_context(
    envelope: &DistributedRunEnvelope,
    resolved: &ResolvedDistributedCapabilities,
) -> ExecutionContext {
    let mut metadata = envelope.task.metadata.clone();
    metadata.insert(
        "_vv_agent_run_id".to_string(),
        serde_json::Value::String(
            envelope
                .root_run_id
                .clone()
                .unwrap_or_else(|| envelope.run_id.clone()),
        ),
    );
    metadata.insert(
        "_vv_agent_trace_id".to_string(),
        serde_json::Value::String(envelope.trace_id.clone().unwrap_or_default()),
    );
    let mut context = ExecutionContext {
        cancellation_token: resolved.cancellation.clone(),
        approval_provider: resolved.approval_provider.clone(),
        approval_broker: resolved.approval_broker.clone(),
        approval_timeout: resolved
            .approval_timeout_seconds
            .map(Duration::from_secs_f64),
        memory_providers: resolved.memory_providers.clone(),
        app_state: resolved.app_state.clone(),
        metadata,
        ..ExecutionContext::default()
    };
    if context.approval_provider.is_some() && context.approval_broker.is_none() {
        context.approval_broker = Some(Default::default());
    }
    context
}

pub(super) fn checkpoint_event_sink(
    resolved: &ResolvedDistributedCapabilities,
) -> CheckpointEventSink {
    let handler = combined_event_handler(resolved);
    Arc::new(move |event| {
        let Some(handler) = handler.as_ref() else {
            return Ok(());
        };
        let value = serde_json::to_value(&event).map_err(|error| error.to_string())?;
        let payload = value
            .as_object()
            .cloned()
            .ok_or_else(|| "checkpoint event must serialize as an object".to_string())?;
        let event_type = payload
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("checkpoint_event")
            .to_string();
        handler(&event_type, &payload.into_iter().collect());
        Ok(())
    })
}

pub(super) fn checkpoint_event_store_adapter(
    envelope: &DistributedRunEnvelope,
    resolved: &ResolvedDistributedCapabilities,
) -> Result<Option<Arc<dyn RunEventStore>>, String> {
    let Some(store) = resolved.checkpoint_event_store.clone() else {
        return Ok(None);
    };
    let store_ref = envelope
        .recipe
        .capabilities
        .checkpoint_event_store_ref
        .clone()
        .ok_or_else(|| {
            "resolved checkpoint event store requires checkpoint_event_store_ref".to_string()
        })?;
    Ok(Some(Arc::new(CheckpointEventStoreAdapter {
        store,
        store_ref,
    })))
}

pub(super) struct CheckpointEventStoreAdapter {
    pub(super) store: Arc<dyn IdempotentRunEventStore>,
    pub(super) store_ref: super::super::CapabilityRef,
}

impl RunEventStore for CheckpointEventStoreAdapter {
    fn append(&self, event: &RunEvent) -> Result<(), EventStoreError> {
        let value = serde_json::to_value(event).map_err(|error| {
            EventStoreError::new("event_store_serialization_error", error.to_string())
        })?;
        let digest = crate::checkpoint::event_payload_digest(&value).map_err(|error| {
            EventStoreError::new("event_store_checkpoint_error", error.to_string())
        })?;
        self.store
            .append_once(event.event_id().as_str(), &digest, &value)
            .map_err(|error| {
                EventStoreError::new("event_store_checkpoint_error", error.to_string())
            })?;
        Ok(())
    }

    fn replay(&self, _query: RunEventReplayQuery) -> Result<RunEventIter, EventStoreError> {
        Ok(Box::new(std::iter::empty()))
    }

    fn append_once(
        &self,
        event_id: &str,
        payload_digest: &str,
        event: &RunEvent,
    ) -> Result<Option<EventCursor>, EventStoreError> {
        let value = serde_json::to_value(event).map_err(|error| {
            EventStoreError::new("event_store_serialization_error", error.to_string())
        })?;
        let appended = self
            .store
            .append_once(event_id, payload_digest, &value)
            .map_err(|error| {
                EventStoreError::new("event_store_checkpoint_error", error.to_string())
            })?;
        Ok(Some(EventCursor::new(
            self.store_ref.clone(),
            appended.cursor,
            Some(event_id.to_string()),
        )))
    }
}
