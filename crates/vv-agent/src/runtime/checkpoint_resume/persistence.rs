//! Checkpoint persistence, extension snapshots, and lease ownership.

use super::*;

impl CheckpointResumeController {
    pub(super) fn acknowledge_terminal(&mut self) -> CheckpointResult<()> {
        let checkpoint = self.require_checkpoint()?.clone();
        if checkpoint.terminal_result.is_none()
            || checkpoint.terminal_acknowledged
            || checkpoint
                .event_outbox
                .iter()
                .any(|entry| entry.state == "pending")
        {
            return Ok(());
        }
        if !self
            .store
            .acknowledge_terminal(&checkpoint.checkpoint_key, checkpoint.revision)?
        {
            self.reload()?;
            if !self.require_checkpoint()?.terminal_acknowledged {
                return Err(CheckpointError::new(
                    "checkpoint_store_conflict",
                    "checkpoint terminal acknowledgement lost its revision",
                ));
            }
            return Ok(());
        }
        self.reload()
    }

    pub(super) fn progress(&mut self) -> CheckpointResult<()> {
        self.assert_heartbeat()?;
        self.snapshot_extensions()?;
        let checkpoint = self.require_checkpoint()?.clone();
        let claim_token = checkpoint.claim_token.clone().ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_claim_active",
                "checkpoint progress requires an active claim",
            )
        })?;
        if !self
            .store
            .progress_checkpoint(checkpoint.clone(), &claim_token, checkpoint.revision)?
        {
            return Err(CheckpointError::new(
                "checkpoint_store_conflict",
                "checkpoint progress lost its claim",
            ));
        }
        self.reload()
    }

    pub(super) fn refresh_snapshot(
        &mut self,
        messages: &[Message],
        cycles: &[CycleRecord],
        shared_state: &Metadata,
        budget_usage: Option<BudgetUsageSnapshot>,
    ) -> CheckpointResult<()> {
        let checkpoint = self.require_checkpoint_mut()?;
        checkpoint.messages = messages.to_vec();
        checkpoint.cycles = cycles.to_vec();
        checkpoint.shared_state = shared_state.clone();
        checkpoint.budget_usage = budget_usage;
        self.snapshot_extensions()
    }

    pub(super) fn set_budget_snapshot(&mut self, budget_usage: Option<BudgetUsageSnapshot>) {
        if let Some(checkpoint) = self.checkpoint.as_mut() {
            checkpoint.budget_usage = budget_usage;
        }
    }

    pub(super) fn snapshot_extensions(&mut self) -> CheckpointResult<()> {
        let mut checkpoint = self.require_checkpoint()?.clone();
        self.snapshot_extensions_into(&mut checkpoint)?;
        self.checkpoint = Some(checkpoint);
        Ok(())
    }

    pub(super) fn snapshot_extensions_into(
        &self,
        checkpoint: &mut Checkpoint,
    ) -> CheckpointResult<()> {
        let mut snapshot = BTreeMap::new();
        for (namespace, extension) in &self.extensions {
            snapshot.insert(
                namespace.clone(),
                ExtensionStateEntry {
                    version: extension.version().to_string(),
                    required: extension.required()
                        || self
                            .config
                            .required_extension_namespaces
                            .contains(namespace),
                    state: extension.snapshot()?,
                },
            );
        }
        for (namespace, entry) in &checkpoint.extension_state {
            snapshot
                .entry(namespace.clone())
                .or_insert_with(|| entry.clone());
        }
        validate_extension_state_size(&snapshot, self.config.max_extension_state_bytes)?;
        checkpoint.extension_state = snapshot;
        Ok(())
    }

    pub(super) fn restore_extensions(&self) -> CheckpointResult<()> {
        let checkpoint = self.require_checkpoint()?;
        for (namespace, entry) in &checkpoint.extension_state {
            let Some(extension) = self.extensions.get(namespace) else {
                if entry.required {
                    return Err(CheckpointError::new(
                        "checkpoint_extension_missing",
                        format!("required checkpoint extension {namespace:?} is unavailable"),
                    ));
                }
                continue;
            };
            if extension.version() != entry.version {
                return Err(CheckpointError::new(
                    "checkpoint_extension_version_mismatch",
                    format!("checkpoint extension {namespace:?} version mismatch"),
                ));
            }
            extension.restore(&entry.state)?;
        }
        for namespace in &self.config.required_extension_namespaces {
            if !checkpoint.extension_state.contains_key(namespace) {
                return Err(CheckpointError::new(
                    "checkpoint_extension_missing",
                    format!("required checkpoint extension {namespace:?} has no durable state"),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn validate_existing_definition(
        &self,
        checkpoint: &Checkpoint,
    ) -> CheckpointResult<()> {
        let stored_digest = run_definition_digest(&checkpoint.run_definition)?;
        if checkpoint.run_definition_digest != stored_digest {
            return Err(CheckpointError::new(
                "checkpoint_definition_mismatch",
                "checkpoint run definition digest does not match its embedded definition",
            ));
        }
        let current_digest = run_definition_digest(&self.run_definition)?;
        if self.run_definition_digest != current_digest {
            return Err(CheckpointError::new(
                "checkpoint_definition_mismatch",
                "current run definition digest does not match its definition",
            ));
        }
        if checkpoint.run_definition != self.run_definition {
            return Err(CheckpointError::new(
                "checkpoint_definition_mismatch",
                "checkpoint embedded run definition does not match this run",
            ));
        }
        Ok(())
    }

    pub(super) fn model_request_projection(
        &self,
        request: &LlmRequest,
        backend: &str,
        model: &str,
    ) -> CheckpointResult<Value> {
        let checkpoint = self.require_checkpoint()?;
        if backend.trim().is_empty() || model.trim().is_empty() {
            return Err(CheckpointError::new(
                "checkpoint_journal_integrity_mismatch",
                "effective model backend and model must be non-empty",
            ));
        }
        let mut settings = request
            .model_settings
            .as_ref()
            .map(|settings| settings.to_value())
            .unwrap_or_else(|| json!({}));
        if let Some(settings) = settings.as_object_mut() {
            settings.remove("timeout_seconds");
        }
        Ok(json!({
            "schema_version": OPERATION_REQUEST_SCHEMA,
            "kind": "model",
            "request": {
                "model": {"backend": backend, "model_id": model},
                "messages": request
                    .messages
                    .iter()
                    .map(|message| message.to_openai_message(true))
                    .collect::<Vec<_>>(),
                "tools": request.tools,
                "settings": settings,
                "output_schema": checkpoint.run_definition.get("output_schema").cloned().unwrap_or(Value::Null),
                "idempotency_key": Value::Null,
            },
        }))
    }

    pub(super) fn find_operation(
        &self,
        kind: OperationKind,
        operation_id: &str,
    ) -> Option<OperationJournalEntry> {
        let checkpoint = self.checkpoint.as_ref()?;
        let journal = match kind {
            OperationKind::Model => &checkpoint.model_call_journal,
            OperationKind::Tool => &checkpoint.tool_journal,
        };
        journal
            .iter()
            .find(|entry| entry.operation_id == operation_id)
            .cloned()
    }

    pub(super) fn find_operation_mut(
        &mut self,
        kind: OperationKind,
        operation_id: &str,
    ) -> CheckpointResult<&mut OperationJournalEntry> {
        let checkpoint = self.require_checkpoint_mut()?;
        let journal = match kind {
            OperationKind::Model => &mut checkpoint.model_call_journal,
            OperationKind::Tool => &mut checkpoint.tool_journal,
        };
        journal
            .iter_mut()
            .find(|entry| entry.operation_id == operation_id)
            .ok_or_else(|| {
                CheckpointError::new(
                    "checkpoint_journal_integrity_mismatch",
                    format!("operation {operation_id:?} is missing from the journal"),
                )
            })
    }

    pub(super) fn find_tool_call(
        &self,
        cycle_index: u32,
        tool_call_id: &str,
    ) -> Option<OperationJournalEntry> {
        self.checkpoint
            .as_ref()?
            .tool_journal
            .iter()
            .find(|entry| {
                entry.cycle_index == u64::from(cycle_index)
                    && entry.tool_call_id.as_deref() == Some(tool_call_id)
            })
            .cloned()
    }

    pub(crate) fn deferred_tool_identity(
        &self,
        cycle_index: u32,
        tool_call_id: &str,
    ) -> Option<(String, String, u64, String)> {
        let entry = self.find_tool_call(cycle_index, tool_call_id)?;
        Some((
            self.checkpoint.as_ref()?.checkpoint_key.clone(),
            entry.operation_id,
            entry.attempt,
            entry.request_digest,
        ))
    }

    pub(crate) fn admit_deferred_batch(
        &mut self,
        entries: &[crate::checkpoint::DeferredBatchEntry],
    ) -> CheckpointResult<crate::checkpoint::DeferredBatchAdmission> {
        let checkpoint = self.require_checkpoint()?.clone();
        let claim_token = checkpoint.claim_token.clone().ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_claim_active",
                "deferred batch admission requires an active claim",
            )
        })?;
        let claimed_cycle = checkpoint.claimed_cycle.ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_claim_active",
                "deferred batch admission requires a claimed cycle",
            )
        })?;
        let admission = self.store.admit_deferred_batch(
            &checkpoint.checkpoint_key,
            checkpoint.revision,
            &claim_token,
            claimed_cycle,
            entries,
        )?;
        self.checkpoint = Some(admission.checkpoint.clone());
        self.owned_claim_token = None;
        self.first_claim_is_recovery = false;
        self.stop_heartbeat();
        self.deliver_pending_outbox()?;
        self.reload()?;
        Ok(crate::checkpoint::DeferredBatchAdmission {
            checkpoint: self.require_checkpoint()?.clone(),
            handles: admission.handles,
        })
    }

    pub(crate) fn accept_deferred_batch(
        &mut self,
        decisions: &[crate::checkpoint::AcceptDeferredDecision],
    ) -> CheckpointResult<crate::checkpoint::DeferredBatchAdmission> {
        let checkpoint = self.require_checkpoint()?.clone();
        let claim_token = checkpoint.claim_token.clone().ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_claim_active",
                "deferred reconciliation requires an active recovery claim",
            )
        })?;
        let claimed_cycle = checkpoint.claimed_cycle.ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_claim_active",
                "deferred reconciliation requires a claimed cycle",
            )
        })?;
        let admission = self.store.accept_deferred_batch(
            &checkpoint.checkpoint_key,
            checkpoint.revision,
            &claim_token,
            claimed_cycle,
            decisions,
        )?;
        self.checkpoint = Some(admission.checkpoint.clone());
        self.owned_claim_token = None;
        self.first_claim_is_recovery = false;
        self.stop_heartbeat();
        self.deliver_pending_outbox()?;
        self.reload()?;
        Ok(crate::checkpoint::DeferredBatchAdmission {
            checkpoint: self.require_checkpoint()?.clone(),
            handles: admission.handles,
        })
    }

    pub(crate) fn deferred_result(
        &self,
        messages: &[Message],
        cycles: &[CycleRecord],
        shared_state: &Metadata,
    ) -> CheckpointResult<AgentResult> {
        let checkpoint = self.require_checkpoint()?;
        Ok(AgentResult {
            status: AgentStatus::Deferred,
            messages: messages.to_vec(),
            cycles: cycles.to_vec(),
            completion_reason: None,
            completion_tool_name: None,
            partial_output: last_assistant_output(cycles),
            budget_usage: checkpoint.budget_usage.clone(),
            budget_exhaustion: None,
            checkpoint_key: Some(checkpoint.checkpoint_key.clone()),
            resume_observation: None,
            final_answer: None,
            wait_reason: Some("deferred_pending".to_string()),
            error: None,
            error_code: None,
            shared_state: shared_state.clone(),
            token_usage: summarize_task_token_usage(&checkpoint.model_calls),
        })
    }

    pub(super) fn find_tool_call_mut(
        &mut self,
        cycle_index: u32,
        tool_call_id: &str,
    ) -> CheckpointResult<&mut OperationJournalEntry> {
        self.require_checkpoint_mut()?
            .tool_journal
            .iter_mut()
            .find(|entry| {
                entry.cycle_index == u64::from(cycle_index)
                    && entry.tool_call_id.as_deref() == Some(tool_call_id)
            })
            .ok_or_else(|| {
                CheckpointError::new(
                    "checkpoint_journal_integrity_mismatch",
                    format!("tool call {tool_call_id:?} is missing from the journal"),
                )
            })
    }

    pub(super) fn unresolved_operation(&self) -> Option<OperationJournalEntry> {
        self.checkpoint.as_ref().and_then(|checkpoint| {
            checkpoint
                .model_call_journal
                .iter()
                .chain(checkpoint.tool_journal.iter())
                .find(|entry| {
                    matches!(
                        entry.state,
                        OperationState::Started | OperationState::Ambiguous
                    )
                })
                .cloned()
        })
    }

    pub(super) fn checkpoint_event(
        &self,
        cycle_index: u32,
        payload: RunEventPayload,
        event_id: String,
    ) -> CheckpointResult<RunEvent> {
        RunEvent::new(
            &self.run_id,
            &self.trace_id,
            &self.agent_name,
            Some(cycle_index),
            payload,
        )
        .with_event_id(event_id)
        .map_err(|error| CheckpointError::new("checkpoint_event_outbox_invalid", error))
    }

    pub(super) fn stable_event_id(
        &self,
        event_type: &str,
        coordinates: &[&str],
    ) -> CheckpointResult<String> {
        Ok(stable_event_id_for(
            self.checkpoint_key()?,
            event_type,
            coordinates,
        ))
    }

    pub(super) fn renew_claim_before_dispatch(&mut self) -> CheckpointResult<()> {
        self.assert_heartbeat()?;
        let checkpoint = self.require_checkpoint()?.clone();
        let claim_token = checkpoint.claim_token.ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_claim_active",
                "external dispatch requires an active checkpoint claim",
            )
        })?;
        let known_expiry = self
            .heartbeat
            .as_ref()
            .map(|heartbeat| heartbeat.lease_expires_at_ms.load(Ordering::Acquire))
            .or(checkpoint.lease_expires_at_ms)
            .ok_or_else(|| {
                CheckpointError::new(
                    "checkpoint_claim_active",
                    "external dispatch requires a leased checkpoint claim",
                )
            })?;
        let expiry = renew_heartbeat_once(
            |expiry, now| {
                self.store.renew_checkpoint_claim(
                    &checkpoint.checkpoint_key,
                    &claim_token,
                    expiry,
                    now,
                )
            },
            self.lease_duration_ms,
            known_expiry,
        )?
        .ok_or_else(|| lease_lost("checkpoint lease renewal failed before external dispatch"))?;
        if let Some(heartbeat) = &self.heartbeat {
            heartbeat
                .lease_expires_at_ms
                .fetch_max(expiry, Ordering::Release);
        }
        self.require_checkpoint_mut()?.lease_expires_at_ms = Some(expiry);
        Ok(())
    }

    pub(super) fn start_heartbeat(&mut self) -> CheckpointResult<()> {
        self.stop_heartbeat();
        let checkpoint = self.require_checkpoint()?.clone();
        let claim_token = checkpoint.claim_token.ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_claim_active",
                "checkpoint heartbeat requires an active claim",
            )
        })?;
        let store = self.store.clone();
        let lease_duration_ms = self.lease_duration_ms;
        let known_expiry = checkpoint.lease_expires_at_ms.ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_claim_active",
                "checkpoint heartbeat requires a leased claim",
            )
        })?;
        let interval = Duration::from_millis((lease_duration_ms / 3).max(10));
        let (stop, stopped) = mpsc::channel();
        let error = Arc::new(Mutex::new(None));
        let lease_expires_at_ms = Arc::new(AtomicU64::new(known_expiry));
        let error_for_thread = error.clone();
        let expiry_for_thread = lease_expires_at_ms.clone();
        let key = checkpoint.checkpoint_key;
        let thread = std::thread::Builder::new()
            .name(format!(
                "vv-agent-checkpoint-{}",
                key.chars().take(32).collect::<String>()
            ))
            .spawn(move || {
                let record_error = |failure| {
                    *error_for_thread
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(failure);
                };
                loop {
                    match stopped.recv_timeout(interval) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    match renew_heartbeat_once(
                        |expiry, now| store.renew_checkpoint_claim(&key, &claim_token, expiry, now),
                        lease_duration_ms,
                        expiry_for_thread.load(Ordering::Acquire),
                    ) {
                        Ok(Some(expiry)) => {
                            let _ = expiry_for_thread.fetch_max(expiry, Ordering::Release);
                        }
                        Ok(None) => {}
                        Err(failure) => {
                            record_error(failure);
                            break;
                        }
                    }
                }
            })
            .map_err(|error| {
                CheckpointError::new(
                    "checkpoint_lease_lost",
                    format!("failed to start checkpoint heartbeat: {error}"),
                )
            })?;
        self.heartbeat = Some(HeartbeatHandle {
            stop,
            error,
            lease_expires_at_ms,
            thread: Some(thread),
        });
        Ok(())
    }

    pub(super) fn stop_heartbeat(&mut self) {
        let Some(mut heartbeat) = self.heartbeat.take() else {
            return;
        };
        let _ = heartbeat.stop.send(());
        if let Some(thread) = heartbeat.thread.take() {
            let _ = thread.join();
        }
    }

    pub(super) fn assert_heartbeat(&self) -> CheckpointResult<()> {
        if let Some(heartbeat) = &self.heartbeat {
            if let Some(error) = heartbeat
                .error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
            {
                return Err(error);
            }
        }
        let Some(checkpoint) = self.checkpoint.as_ref() else {
            return Ok(());
        };
        let Some(claim_token) = checkpoint.claim_token.as_deref() else {
            return Ok(());
        };
        if self.owned_claim_token.as_deref() != Some(claim_token) {
            return Err(lease_lost("checkpoint claim is no longer locally owned"));
        }
        let lease_expires_at_ms = self
            .heartbeat
            .as_ref()
            .map(|heartbeat| heartbeat.lease_expires_at_ms.load(Ordering::Acquire))
            .or(checkpoint.lease_expires_at_ms)
            .ok_or_else(|| lease_lost("checkpoint lease is no longer active"))?;
        if lease_expires_at_ms <= now_ms()? {
            return Err(lease_lost("checkpoint lease expired locally"));
        }
        Ok(())
    }

    pub(super) fn reload(&mut self) -> CheckpointResult<()> {
        let key = self.checkpoint_key()?.to_string();
        self.checkpoint = self.store.load_checkpoint(&key)?;
        if self.checkpoint.is_none() {
            return Err(CheckpointError::new(
                "checkpoint_not_found",
                "checkpoint disappeared from its store",
            ));
        }
        Ok(())
    }

    pub(super) fn require_checkpoint(&self) -> CheckpointResult<&Checkpoint> {
        self.checkpoint.as_ref().ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_not_admitted",
                "checkpoint controller has not been admitted",
            )
        })
    }

    pub(super) fn require_checkpoint_mut(&mut self) -> CheckpointResult<&mut Checkpoint> {
        self.checkpoint.as_mut().ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_not_admitted",
                "checkpoint controller has not been admitted",
            )
        })
    }
}

fn lease_lost(message: &'static str) -> CheckpointError {
    CheckpointError::new("checkpoint_lease_lost", message)
}

fn renew_heartbeat_once(
    renew: impl FnOnce(u64, u64) -> CheckpointResult<bool>,
    lease_duration_ms: u64,
    known_expiry: u64,
) -> CheckpointResult<Option<u64>> {
    let now = now_ms()?;
    if now >= known_expiry {
        return Err(lease_lost(
            "checkpoint lease expired before heartbeat renewal",
        ));
    }
    let expiry = now.checked_add(lease_duration_ms).ok_or_else(|| {
        CheckpointError::new("checkpoint_claim_invalid", "checkpoint lease overflow")
    })?;
    match renew(expiry, now) {
        Ok(false) => Err(lease_lost("checkpoint heartbeat lost its claim")),
        Ok(true) => {
            let observed = now_ms()?;
            if observed >= known_expiry || observed >= expiry {
                Err(lease_lost(
                    "checkpoint lease expired during heartbeat renewal",
                ))
            } else {
                Ok(Some(expiry))
            }
        }
        Err(_) => (now_ms()? < known_expiry)
            .then_some(None)
            .ok_or_else(|| lease_lost("checkpoint lease expired during heartbeat renewal")),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    use super::*;
    use crate::InMemoryCheckpointStore;

    struct RecordingExtension {
        snapshots: Arc<AtomicUsize>,
    }

    impl CheckpointExtension for RecordingExtension {
        fn namespace(&self) -> &str {
            "test"
        }

        fn version(&self) -> &str {
            "1"
        }

        fn required(&self) -> bool {
            false
        }

        fn snapshot(&self) -> CheckpointResult<Value> {
            self.snapshots.fetch_add(1, Ordering::SeqCst);
            Ok(Value::Null)
        }

        fn restore(&self, _state: &Value) -> CheckpointResult<()> {
            Ok(())
        }
    }

    #[test]
    fn transient_store_error_does_not_poison_heartbeat_retry() {
        let now = now_ms().expect("current time");
        let known_expiry = now + 10_000;

        assert_eq!(
            renew_heartbeat_once(
                |_, _| {
                    Err(CheckpointError::new(
                        "test_transient_store_error",
                        "injected transient renewal failure",
                    ))
                },
                1_000,
                known_expiry,
            )
            .expect("transient store error is retryable"),
            None
        );
        assert!(renew_heartbeat_once(|_, _| Ok(true), 1_000, known_expiry)
            .expect("renewal retry")
            .is_some());

        let false_error = renew_heartbeat_once(|_, _| Ok(false), 1_000, known_expiry)
            .expect_err("false renewal must fail closed");
        assert_eq!(false_error.code(), "checkpoint_lease_lost");

        let expired_error = renew_heartbeat_once(|_, _| Ok(true), 1_000, 0)
            .expect_err("expired lease must fail closed");
        assert_eq!(expired_error.code(), "checkpoint_lease_lost");
    }

    #[test]
    fn external_dispatch_rejects_renewal_returned_after_lease_expiry() {
        let now = now_ms().expect("current time");
        let error = renew_heartbeat_once(
            |_, _| {
                std::thread::sleep(Duration::from_millis(100));
                Ok(true)
            },
            1_000,
            now + 50,
        )
        .expect_err("delayed renewal must fail closed");
        assert_eq!(error.code(), "checkpoint_lease_lost");
    }

    #[test]
    fn progress_checks_heartbeat_before_snapshotting_extensions() {
        let snapshots = Arc::new(AtomicUsize::new(0));
        let mut controller = CheckpointResumeController::new(CheckpointControllerRequest {
            config: CheckpointConfig::with_store(InMemoryCheckpointStore::new()),
            task_id: "task".to_string(),
            run_id: "run".to_string(),
            trace_id: "trace".to_string(),
            agent_name: "agent".to_string(),
            run_definition: json!({}),
            run_definition_digest: String::new(),
            initial_messages: Vec::new(),
            initial_shared_state: BTreeMap::new(),
            initial_budget_usage: None,
            extensions: vec![Arc::new(RecordingExtension {
                snapshots: snapshots.clone(),
            })],
            reconciliation_provider: None,
            event_sink: Arc::new(|_| Ok(())),
            event_store: None,
            preloaded_checkpoint: None,
        })
        .expect("controller");
        controller.checkpoint = Some(Checkpoint::default());
        let (stop, _stopped) = mpsc::channel();
        controller.heartbeat = Some(HeartbeatHandle {
            stop,
            error: Arc::new(Mutex::new(Some(CheckpointError::new(
                "checkpoint_lease_lost",
                "injected heartbeat failure",
            )))),
            lease_expires_at_ms: Arc::new(AtomicU64::new(0)),
            thread: None,
        });

        let error = controller
            .progress()
            .expect_err("failed heartbeat must stop progress");
        assert_eq!(error.code(), "checkpoint_lease_lost");
        assert_eq!(snapshots.load(Ordering::SeqCst), 0);
    }
}
