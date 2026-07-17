//! Controller entry points for checkpointed execution.

use super::*;

impl CheckpointResumeController {
    pub(crate) fn new(request: CheckpointControllerRequest) -> CheckpointResult<Self> {
        request.config.validate()?;
        let store = request.config.store.clone().ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_store_unavailable",
                "process-local checkpoint execution requires CheckpointConfig.store",
            )
        })?;
        let mut extensions = BTreeMap::new();
        for extension in request.extensions {
            if extensions
                .insert(extension.namespace().to_string(), extension)
                .is_some()
            {
                return Err(CheckpointError::new(
                    "checkpoint_extension_namespace_duplicate",
                    "checkpoint extension namespaces must be unique",
                ));
            }
        }
        Ok(Self {
            config: request.config,
            store,
            task_id: request.task_id,
            run_id: request.run_id,
            trace_id: request.trace_id,
            agent_name: request.agent_name,
            run_definition: request.run_definition,
            run_definition_digest: request.run_definition_digest,
            initial_messages: request.initial_messages,
            initial_shared_state: request.initial_shared_state,
            initial_budget_usage: request.initial_budget_usage,
            extensions,
            reconciliation_provider: request.reconciliation_provider,
            event_sink: request.event_sink,
            event_store: request.event_store,
            preloaded_checkpoint: request.preloaded_checkpoint,
            checkpoint: None,
            created: false,
            first_claim_is_recovery: false,
            owned_claim_token: None,
            lease_duration_ms: DEFAULT_CHECKPOINT_LEASE_MS,
            heartbeat: None,
        })
    }

    pub(crate) fn checkpoint_key(&self) -> CheckpointResult<&str> {
        Ok(&self.require_checkpoint()?.checkpoint_key)
    }

    pub(crate) fn checkpoint(&self) -> CheckpointResult<&CheckpointV2> {
        self.require_checkpoint()
    }

    pub(crate) fn checkpoint_config(&self) -> &CheckpointConfig {
        &self.config
    }

    pub(crate) fn next_claim_mode(&self) -> ClaimMode {
        if self.first_claim_is_recovery {
            ClaimMode::Recovery
        } else {
            ClaimMode::Continue
        }
    }

    pub(crate) fn set_next_claim_mode(&mut self, claim_mode: ClaimMode) {
        self.first_claim_is_recovery = claim_mode == ClaimMode::Recovery;
    }

    pub(crate) fn set_lease_duration_ms(&mut self, lease_duration_ms: u64) -> CheckpointResult<()> {
        if lease_duration_ms == 0 {
            return Err(CheckpointError::new(
                "checkpoint_config_invalid",
                "checkpoint lease duration must be positive",
            ));
        }
        self.lease_duration_ms = lease_duration_ms;
        Ok(())
    }

    pub(crate) fn refresh_authoritative(&mut self) -> CheckpointResult<CheckpointV2> {
        self.reload()?;
        self.validate_existing_definition(self.require_checkpoint()?)?;
        Ok(self.require_checkpoint()?.clone())
    }

    pub(crate) fn adopt_claim_for_terminal_finalize(
        &mut self,
        claim_token: &str,
        lease_duration_ms: u64,
    ) -> CheckpointResult<()> {
        if claim_token.trim().is_empty() {
            return Err(CheckpointError::new(
                "checkpoint_claim_invalid",
                "distributed terminal claim token must be non-empty",
            ));
        }
        self.set_lease_duration_ms(lease_duration_ms)?;
        self.reload()?;
        let checkpoint = self.require_checkpoint()?;
        if checkpoint.terminal_result.is_some()
            || checkpoint.claim_token.as_deref() != Some(claim_token)
            || checkpoint.claimed_cycle.is_none()
        {
            return Err(CheckpointError::new(
                "checkpoint_claim_active",
                "distributed terminal claim no longer matches the durable checkpoint",
            ));
        }
        self.owned_claim_token = Some(claim_token.to_string());
        self.renew_claim_before_dispatch()?;
        self.start_heartbeat()
    }

    pub(crate) fn assert_heartbeat_healthy(&self) -> CheckpointResult<()> {
        self.assert_heartbeat()
    }

    pub(crate) fn replay_terminal_if_present(&mut self) -> CheckpointResult<Option<AgentResult>> {
        self.reload()?;
        let Some(terminal) = self.require_checkpoint()?.terminal_result.clone() else {
            return Ok(None);
        };
        self.deliver_pending_outbox()?;
        self.acknowledge_terminal()?;
        let mut result = AgentResult::from_dict(&terminal)
            .map_err(|error| CheckpointError::new("checkpoint_terminal_result_invalid", error))?;
        result.checkpoint_key = Some(self.checkpoint_key()?.to_string());
        Ok(Some(result))
    }

    pub(crate) fn admit(&mut self) -> CheckpointResult<Option<AgentResult>> {
        let key = self
            .config
            .key
            .clone()
            .unwrap_or_else(|| format!("checkpoint_{}", uuid::Uuid::new_v4().simple()));
        self.config.key = Some(key.clone());

        let mut existing = self.preloaded_checkpoint.take();
        if existing
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.checkpoint_key != key)
        {
            return Err(CheckpointError::new(
                "checkpoint_key_conflict",
                "preloaded checkpoint key does not match CheckpointConfig.key",
            ));
        }
        if existing.is_none() {
            existing = self.store.load_checkpoint_v2(&key)?;
        }

        if self.config.resume_policy == ResumePolicy::New {
            if existing.is_some() {
                return Err(CheckpointError::new(
                    "checkpoint_key_conflict",
                    format!("checkpoint key {key:?} already exists"),
                ));
            }
            self.create_new_checkpoint(key)?;
            return Ok(None);
        }

        if existing.is_none() {
            if self.config.resume_policy == ResumePolicy::RequireExisting {
                return Err(CheckpointError::new(
                    "checkpoint_not_found",
                    format!("checkpoint key {key:?} does not exist"),
                ));
            }
            if self.create_new_checkpoint(key.clone()).is_ok() {
                return Ok(None);
            }
            existing = self.store.load_checkpoint_v2(&key)?;
        }

        let existing = existing.ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_store_conflict",
                "checkpoint disappeared after a concurrent create",
            )
        })?;
        self.validate_existing_definition(&existing)?;
        self.checkpoint = Some(existing);

        if self.require_checkpoint()?.terminal_result.is_some() {
            self.deliver_pending_outbox()?;
            self.acknowledge_terminal()?;
            let checkpoint = self.require_checkpoint()?;
            let mut result = AgentResult::from_dict(
                checkpoint
                    .terminal_result
                    .as_ref()
                    .expect("terminal checked above"),
            )
            .map_err(|error| CheckpointError::new("checkpoint_terminal_result_invalid", error))?;
            result.checkpoint_key = Some(checkpoint.checkpoint_key.clone());
            return Ok(Some(result));
        }

        let now_ms = now_ms()?;
        if self
            .require_checkpoint()?
            .lease_expires_at_ms
            .is_some_and(|expiry| expiry > now_ms)
        {
            return Err(CheckpointError::new(
                "checkpoint_claim_active",
                format!("checkpoint key {key:?} has a live claim"),
            ));
        }
        self.restore_extensions()?;
        self.first_claim_is_recovery = true;
        Ok(None)
    }

    pub(crate) fn begin_cycle(
        &mut self,
        cycle_index: u32,
    ) -> CheckpointResult<Option<AgentResult>> {
        self.ensure_claim(u64::from(cycle_index))
    }

    pub(crate) fn complete_model<F>(
        &mut self,
        cycle_index: u32,
        operation_slot: &str,
        request: &LlmRequest,
        budget_usage: Option<BudgetUsageSnapshot>,
        invoke: F,
    ) -> CheckpointResult<ModelOperationOutcome>
    where
        F: FnOnce() -> Result<LLMResponse, LlmError>,
    {
        if operation_slot.trim().is_empty() {
            return Err(CheckpointError::new(
                "checkpoint_journal_integrity_mismatch",
                "model operation slot must be non-empty",
            ));
        }
        if let Some(interruption) = self.ensure_claim(u64::from(cycle_index))? {
            return Ok(ModelOperationOutcome::Interrupted(Box::new(interruption)));
        }
        self.set_budget_snapshot(budget_usage);
        let projection = self.model_request_projection(request)?;
        let digest = operation_request_digest(OperationKind::Model, &projection)?;
        let operation_id = model_operation_id(cycle_index, operation_slot);

        if let Some(entry) = self.find_operation(OperationKind::Model, &operation_id) {
            if entry.request_digest != digest {
                return Err(CheckpointError::new(
                    "checkpoint_journal_integrity_mismatch",
                    "model request does not match the durable operation slot",
                ));
            }
            match entry.state {
                OperationState::Succeeded => {
                    let response = entry.response.clone().ok_or_else(|| {
                        CheckpointError::new(
                            "checkpoint_journal_integrity_mismatch",
                            "durable model response is missing",
                        )
                    })?;
                    self.emit_operation_replayed(&entry)?;
                    let response = serde_json::from_value(response).map_err(|error| {
                        CheckpointError::new(
                            "checkpoint_journal_integrity_mismatch",
                            format!("durable model response is invalid: {error}"),
                        )
                    })?;
                    return Ok(ModelOperationOutcome::Response(Box::new(response)));
                }
                OperationState::Failed => {
                    let error = entry.error.clone().unwrap_or_else(|| {
                        OperationError::new(
                            "model_request_failed",
                            "durable model operation failed",
                            false,
                        )
                    });
                    self.emit_operation_replayed(&entry)?;
                    return Ok(ModelOperationOutcome::Error(LlmError::Request(format!(
                        "{}: {}",
                        error.code, error.message
                    ))));
                }
                OperationState::Planned => {}
                _ => {
                    return Err(CheckpointError::new(
                        "checkpoint_journal_integrity_mismatch",
                        "model journal is not executable after recovery",
                    ));
                }
            }
        } else {
            let entry = OperationJournalEntry::model(
                operation_id.clone(),
                u64::from(cycle_index),
                1,
                digest,
                None,
            );
            self.require_checkpoint_mut()?
                .model_call_journal
                .push(entry);
            self.progress()?;
        }

        {
            let entry = self.find_operation_mut(OperationKind::Model, &operation_id)?;
            entry.state = OperationState::Started;
            entry.validate()?;
        }
        self.progress()?;
        self.renew_claim_before_dispatch()?;

        let outcome = catch_unwind(AssertUnwindSafe(invoke));
        match outcome {
            Ok(Ok(response)) => {
                let receipt = serde_json::to_value(&response).map_err(|error| {
                    CheckpointError::new(
                        "checkpoint_journal_integrity_mismatch",
                        format!("model response cannot be serialized: {error}"),
                    )
                })?;
                let entry = self.find_operation_mut(OperationKind::Model, &operation_id)?;
                entry.state = OperationState::Succeeded;
                entry.response = Some(receipt);
                entry.error = None;
                entry.validate()?;
                self.progress()?;
                Ok(ModelOperationOutcome::Response(Box::new(response)))
            }
            Ok(Err(error)) if definitive_model_error(&error) => {
                let entry = self.find_operation_mut(OperationKind::Model, &operation_id)?;
                entry.state = OperationState::Failed;
                entry.error = Some(OperationError::new(
                    "model_request_failed",
                    error.to_string(),
                    false,
                ));
                entry.validate()?;
                self.progress()?;
                Ok(ModelOperationOutcome::Error(error))
            }
            Ok(Err(_)) | Err(_) => {
                let entry = self.find_operation_mut(OperationKind::Model, &operation_id)?;
                entry.state = OperationState::Ambiguous;
                entry.validate()?;
                self.progress()?;
                let entry = self
                    .find_operation(OperationKind::Model, &operation_id)
                    .expect("model operation remains present");
                Ok(ModelOperationOutcome::Interrupted(Box::new(
                    self.suspend_for(&entry)?,
                )))
            }
        }
    }

    pub(crate) fn plan_tool(
        &mut self,
        cycle_index: u32,
        call: &ToolCall,
        idempotency_support: ToolIdempotency,
        budget_usage: Option<BudgetUsageSnapshot>,
    ) -> CheckpointResult<(ToolOperationPlan, Option<AgentResult>)> {
        if let Some(interruption) = self.ensure_claim(u64::from(cycle_index))? {
            return Ok((
                ToolOperationPlan {
                    idempotency_key: String::new(),
                    replay_result: None,
                },
                Some(interruption),
            ));
        }
        self.set_budget_snapshot(budget_usage);
        let idempotency_key = tool_idempotency_key(self.checkpoint_key()?, cycle_index, &call.id);
        let projection = json!({
            "schema_version": OPERATION_REQUEST_SCHEMA,
            "kind": "tool",
            "request": {
                "tool_call_id": call.id,
                "tool_name": call.name,
                "arguments": call.arguments,
                "idempotency_key": idempotency_key,
            },
        });
        let digest = operation_request_digest(OperationKind::Tool, &projection)?;

        if let Some(entry) = self.find_tool_call(cycle_index, &call.id) {
            entry.verify_request(&projection)?;
            if entry.idempotency_support != Some(idempotency_support) {
                return Err(CheckpointError::new(
                    "checkpoint_journal_integrity_mismatch",
                    "tool idempotency declaration changed after checkpoint creation",
                ));
            }
            match entry.state {
                OperationState::Succeeded => {
                    let result =
                        ToolExecutionResult::from_dict(entry.result.as_ref().ok_or_else(|| {
                            CheckpointError::new(
                                "checkpoint_journal_integrity_mismatch",
                                "durable tool result is missing",
                            )
                        })?)
                        .map_err(|error| {
                            CheckpointError::new("checkpoint_journal_integrity_mismatch", error)
                        })?;
                    self.emit_operation_replayed(&entry)?;
                    return Ok((
                        ToolOperationPlan {
                            idempotency_key,
                            replay_result: Some(result),
                        },
                        None,
                    ));
                }
                OperationState::Failed => {
                    let error = entry.error.clone().unwrap_or_else(|| {
                        OperationError::new(
                            "tool_operation_failed",
                            "durable tool operation failed",
                            false,
                        )
                    });
                    self.emit_operation_replayed(&entry)?;
                    return Ok((
                        ToolOperationPlan {
                            idempotency_key,
                            replay_result: Some(ToolExecutionResult {
                                tool_call_id: call.id.clone(),
                                content: error.message,
                                status: ToolResultStatus::Error,
                                directive: crate::types::ToolDirective::Continue,
                                error_code: Some(error.code),
                                metadata: Metadata::new(),
                                image_url: None,
                                image_path: None,
                            }),
                        },
                        None,
                    ));
                }
                OperationState::Planned => {
                    return Ok((
                        ToolOperationPlan {
                            idempotency_key,
                            replay_result: None,
                        },
                        None,
                    ));
                }
                _ => {
                    return Err(CheckpointError::new(
                        "checkpoint_journal_integrity_mismatch",
                        "tool journal is not executable after recovery",
                    ));
                }
            }
        }

        let operation_id = format!(
            "op_tool_cycle_{}_call_{}",
            cycle_index,
            self.require_checkpoint()?.tool_journal.len() + 1
        );
        self.require_checkpoint_mut()?
            .tool_journal
            .push(OperationJournalEntry::tool(
                operation_id,
                u64::from(cycle_index),
                1,
                digest,
                call.id.clone(),
                call.name.clone(),
                call.arguments.clone().into_iter().collect(),
                idempotency_key.clone(),
                idempotency_support,
            ));
        self.progress()?;
        Ok((
            ToolOperationPlan {
                idempotency_key,
                replay_result: None,
            },
            None,
        ))
    }

    pub(crate) fn tool_started(
        &mut self,
        cycle_index: u32,
        call: &ToolCall,
    ) -> CheckpointResult<()> {
        let entry = self.find_tool_call_mut(cycle_index, &call.id)?;
        if entry.state == OperationState::Planned {
            entry.state = OperationState::Started;
            entry.validate()?;
            self.progress()?;
            self.renew_claim_before_dispatch()?;
        }
        Ok(())
    }

    pub(crate) fn finish_tool(
        &mut self,
        cycle_index: u32,
        call: &ToolCall,
        result: &ToolExecutionResult,
        budget_usage: Option<BudgetUsageSnapshot>,
    ) -> CheckpointResult<Option<AgentResult>> {
        self.set_budget_snapshot(budget_usage);
        let state = self
            .find_tool_call(cycle_index, &call.id)
            .ok_or_else(|| {
                CheckpointError::new(
                    "checkpoint_journal_integrity_mismatch",
                    format!("tool call {:?} is missing from the journal", call.id),
                )
            })?
            .state;
        if state == OperationState::Planned {
            if result.error_code.as_deref() == Some("tool_approval_required") {
                return Ok(None);
            }
            let entry = self.find_tool_call_mut(cycle_index, &call.id)?;
            entry.state = OperationState::Failed;
            entry.error = Some(OperationError::new(
                result
                    .error_code
                    .clone()
                    .unwrap_or_else(|| "tool_short_circuited".to_string()),
                if result.content.is_empty() {
                    "tool invocation was short-circuited".to_string()
                } else {
                    result.content.clone()
                },
                false,
            ));
            entry.validate()?;
            self.progress()?;
            return Ok(None);
        }
        if state != OperationState::Started {
            return Ok(None);
        }

        let definitive = result
            .metadata
            .get("definitive_outcome")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let ambiguous_code = result.error_code.as_deref().is_some_and(|code| {
            matches!(
                code,
                "tool_timeout"
                    | "tool_cancelled"
                    | "tool_connection_lost"
                    | "tool_execution_failed"
                    | "tool_orchestrator_error"
            )
        });
        if ambiguous_code && !definitive {
            let entry = self.find_tool_call_mut(cycle_index, &call.id)?;
            entry.state = OperationState::Ambiguous;
            entry.validate()?;
            self.progress()?;
            let entry = self.find_tool_call(cycle_index, &call.id).ok_or_else(|| {
                CheckpointError::new(
                    "checkpoint_journal_integrity_mismatch",
                    format!("tool call {:?} is missing from the journal", call.id),
                )
            })?;
            return Ok(Some(self.suspend_for(&entry)?));
        }

        let entry = self.find_tool_call_mut(cycle_index, &call.id)?;
        if matches!(
            result.status,
            ToolResultStatus::Success | ToolResultStatus::WaitResponse
        ) {
            entry.state = OperationState::Succeeded;
            entry.result = Some(result.to_dict());
            entry.error = None;
        } else {
            entry.state = OperationState::Failed;
            entry.result = None;
            entry.error = Some(OperationError::new(
                result
                    .error_code
                    .clone()
                    .unwrap_or_else(|| "tool_operation_failed".to_string()),
                if result.content.is_empty() {
                    "tool operation failed".to_string()
                } else {
                    result.content.clone()
                },
                result
                    .metadata
                    .get("retryable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            ));
        }
        entry.validate()?;
        self.progress()?;
        Ok(None)
    }

    pub(crate) fn update_budget_usage(
        &mut self,
        budget_usage: Option<BudgetUsageSnapshot>,
    ) -> CheckpointResult<()> {
        self.set_budget_snapshot(budget_usage);
        if self.require_checkpoint()?.claim_token.is_some() {
            self.progress()?;
        }
        Ok(())
    }

    pub(crate) fn commit_cycle(
        &mut self,
        cycle_index: u32,
        messages: &[Message],
        cycles: &[CycleRecord],
        shared_state: &Metadata,
        budget_usage: Option<BudgetUsageSnapshot>,
    ) -> CheckpointResult<()> {
        if self.require_checkpoint()?.claim_token.is_none() {
            return Ok(());
        }
        if cycles.last().map(|cycle| cycle.index) != Some(cycle_index) {
            return Err(CheckpointError::new(
                "checkpoint_cycle_conflict",
                "cannot commit a checkpoint without the completed cycle record",
            ));
        }
        self.refresh_snapshot(messages, cycles, shared_state, budget_usage)?;
        let checkpoint = self.require_checkpoint_mut()?;
        checkpoint.cycle_index = u64::from(cycle_index);
        checkpoint.status = CheckpointStatus::Running;
        checkpoint
            .event_outbox
            .retain(|entry| entry.state == "pending");
        let revision = checkpoint.revision;
        let claim_token = checkpoint
            .claim_token
            .clone()
            .expect("active claim checked above");
        if !self.store.commit_checkpoint_v2(
            self.require_checkpoint()?.clone(),
            &claim_token,
            revision,
        )? {
            return Err(CheckpointError::new(
                "checkpoint_store_conflict",
                "checkpoint cycle commit lost its claim",
            ));
        }
        self.checkpoint = self.store.load_checkpoint_v2(self.checkpoint_key()?)?;
        self.first_claim_is_recovery = false;
        self.owned_claim_token = None;
        self.stop_heartbeat();
        Ok(())
    }

    pub(crate) fn prepare_terminal(
        &mut self,
        mut result: AgentResult,
    ) -> CheckpointResult<AgentResult> {
        result.checkpoint_key = Some(self.checkpoint_key()?.to_string());
        if result.status == AgentStatus::ReconciliationRequired || is_operator_abort(&result) {
            return Ok(result);
        }
        self.reload()?;
        if let Some(terminal) = self.require_checkpoint()?.terminal_result.as_ref() {
            return AgentResult::from_dict(terminal).map_err(|error| {
                CheckpointError::new("checkpoint_terminal_result_invalid", error)
            });
        }
        let unresolved = self.unresolved_operation();
        let Some(mut unresolved) = unresolved else {
            return Ok(result);
        };
        if unresolved.state == OperationState::Started {
            let entry = self.find_operation_mut(unresolved.kind, &unresolved.operation_id)?;
            entry.state = OperationState::Ambiguous;
            entry.validate()?;
            self.progress()?;
            unresolved.state = OperationState::Ambiguous;
        }
        self.suspend_for(&unresolved)
    }

    pub(crate) fn persist_preterminal_event(
        &mut self,
        event: RunEvent,
        identity: &str,
    ) -> CheckpointResult<()> {
        let event_id = self.stable_event_id("preterminal", &[identity, "0"])?;
        let event = event
            .with_event_id(event_id)
            .map_err(|error| CheckpointError::new("checkpoint_event_outbox_invalid", error))?;
        self.emit_durable(event)
    }

    pub(crate) fn finalize(
        &mut self,
        mut result: AgentResult,
        terminal_event: Option<RunEvent>,
    ) -> CheckpointResult<AgentResult> {
        if result.status == AgentStatus::ReconciliationRequired {
            result.checkpoint_key = Some(self.checkpoint_key()?.to_string());
            return Ok(result);
        }
        self.reload()?;
        if self.require_checkpoint()?.terminal_result.is_some() {
            self.deliver_pending_outbox()?;
            self.acknowledge_terminal()?;
            return AgentResult::from_dict(
                self.require_checkpoint()?
                    .terminal_result
                    .as_ref()
                    .expect("terminal checked"),
            )
            .map_err(|error| CheckpointError::new("checkpoint_terminal_result_invalid", error));
        }
        if self.unresolved_operation().is_some() && !is_operator_abort(&result) {
            return Err(CheckpointError::new(
                "checkpoint_terminal_unresolved_operation",
                "checkpoint terminal finalization has an unresolved operation",
            ));
        }

        if result.status == AgentStatus::Failed {
            if let Some(active_claim) = self.require_checkpoint()?.claim_token.as_deref() {
                if self.owned_claim_token.as_deref() != Some(active_claim) {
                    // A worker may still have a durable outcome to commit. Preserve the
                    // scheduler error and leave the checkpoint recoverable by its owner.
                    result.checkpoint_key = Some(self.checkpoint_key()?.to_string());
                    self.stop_heartbeat();
                    return Ok(result);
                }
            }
        }

        result.checkpoint_key = Some(self.checkpoint_key()?.to_string());
        let status = checkpoint_status(result.status)?;
        {
            let checkpoint = self.require_checkpoint_mut()?;
            if let Some(terminal_cycle) = result.cycles.last().map(|cycle| u64::from(cycle.index)) {
                if terminal_cycle < checkpoint.cycle_index
                    || checkpoint
                        .claimed_cycle
                        .is_some_and(|claimed_cycle| claimed_cycle != terminal_cycle)
                {
                    return Err(CheckpointError::new(
                        "checkpoint_cycle_conflict",
                        "terminal result cycle does not match the durable checkpoint claim",
                    ));
                }
                checkpoint.cycle_index = terminal_cycle;
            }
            checkpoint.status = status;
            checkpoint.terminal_result = Some(result.to_dict());
            checkpoint.messages = result.messages.clone();
            checkpoint.cycles = result.cycles.clone();
            checkpoint.shared_state = result.shared_state.clone();
            checkpoint.budget_usage = result.budget_usage.clone();
            checkpoint
                .event_outbox
                .retain(|entry| entry.state == "pending");
        }
        self.snapshot_extensions()?;
        if let Some(event) = terminal_event {
            let event_id = self.stable_event_id(
                "terminal",
                &[
                    event_type(&event),
                    &event.cycle_index().unwrap_or(0).to_string(),
                ],
            )?;
            let event = event
                .with_event_id(event_id)
                .map_err(|error| CheckpointError::new("checkpoint_event_outbox_invalid", error))?;
            self.queue_outbox_event(event)?;
        }
        let checkpoint = self.require_checkpoint()?.clone();
        let revision = checkpoint.revision;
        let claim_token = checkpoint.claim_token.clone();
        if let Some(claim_token) = claim_token.as_deref() {
            match self.owned_claim_token.as_deref() {
                Some(owned) if owned == claim_token => {}
                Some(_) => {
                    return Err(CheckpointError::new(
                        "checkpoint_claim_active",
                        "checkpoint claim changed after terminal ownership was adopted",
                    ))
                }
                None => {
                    return Err(CheckpointError::new(
                        "checkpoint_claim_active",
                        "checkpoint terminal finalization has not adopted the active claim",
                    ))
                }
            }
        }
        let finalized = match claim_token.as_deref() {
            Some(claim_token) => {
                self.store
                    .finalize_claimed_v2(checkpoint, claim_token, revision)?
            }
            None => self.store.finalize_checkpoint_v2(checkpoint, revision)?,
        };
        if !finalized {
            self.reload()?;
            if self.require_checkpoint()?.terminal_result.is_none() {
                return Err(CheckpointError::new(
                    "checkpoint_store_conflict",
                    "checkpoint terminal finalization lost its revision",
                ));
            }
        } else {
            self.reload()?;
        }
        self.stop_heartbeat();
        self.deliver_pending_outbox()?;
        self.acknowledge_terminal()?;
        result.checkpoint_key = Some(self.checkpoint_key()?.to_string());
        Ok(result)
    }

    pub(crate) fn close(&mut self) {
        self.stop_heartbeat();
    }
}
