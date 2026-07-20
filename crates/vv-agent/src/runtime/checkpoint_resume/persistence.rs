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
            .acknowledge_terminal_v2(&checkpoint.checkpoint_key, checkpoint.revision)?
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
        if !self.store.progress_checkpoint_v2(
            checkpoint.clone(),
            &claim_token,
            checkpoint.revision,
        )? {
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
        checkpoint: &mut CheckpointV2,
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
        checkpoint: &CheckpointV2,
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
        if run_definition_comparison_copy(&checkpoint.run_definition)
            != run_definition_comparison_copy(&self.run_definition)
        {
            return Err(CheckpointError::new(
                "checkpoint_definition_mismatch",
                "checkpoint embedded run definition does not match this run",
            ));
        }
        Ok(())
    }

    pub(super) fn model_request_projection(&self, request: &LlmRequest) -> CheckpointResult<Value> {
        let checkpoint = self.require_checkpoint()?;
        let model_definition = checkpoint
            .run_definition
            .get("model")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                CheckpointError::new(
                    "checkpoint_definition_invalid",
                    "checkpoint model definition is invalid",
                )
            })?;
        let model_override = request
            .metadata
            .get("_vv_agent_checkpoint_model")
            .and_then(Value::as_object);
        let backend = model_override
            .and_then(|value| value.get("backend"))
            .or_else(|| model_definition.get("backend"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CheckpointError::new(
                    "checkpoint_journal_integrity_mismatch",
                    "effective model backend is unavailable",
                )
            })?;
        let model_id = model_override
            .and_then(|value| value.get("model_id"))
            .or_else(|| model_definition.get("model_id"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                CheckpointError::new(
                    "checkpoint_journal_integrity_mismatch",
                    "effective model id is unavailable",
                )
            })?;
        let settings = if model_override.is_some() {
            request
                .model_settings
                .as_ref()
                .map(|settings| settings.to_value())
                .unwrap_or_else(|| json!({}))
        } else {
            model_definition
                .get("settings")
                .cloned()
                .unwrap_or_else(|| json!({}))
        };
        Ok(json!({
            "schema_version": OPERATION_REQUEST_SCHEMA,
            "kind": "model",
            "request": {
                "model": {"backend": backend, "model_id": model_id},
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
        let now = now_ms()?;
        let expiry = now.checked_add(self.lease_duration_ms).ok_or_else(|| {
            CheckpointError::new("checkpoint_claim_invalid", "checkpoint lease overflow")
        })?;
        if !self.store.renew_checkpoint_claim_v2(
            &checkpoint.checkpoint_key,
            &claim_token,
            expiry,
            now,
        )? {
            return Err(CheckpointError::new(
                "checkpoint_lease_lost",
                "checkpoint lease renewal lost its claim before external dispatch",
            ));
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
        let interval = Duration::from_millis((lease_duration_ms / 3).max(10));
        let (stop, stopped) = mpsc::channel();
        let error = Arc::new(Mutex::new(None));
        let error_for_thread = error.clone();
        let key = checkpoint.checkpoint_key;
        let thread = std::thread::Builder::new()
            .name(format!(
                "vv-agent-checkpoint-{}",
                key.chars().take(32).collect::<String>()
            ))
            .spawn(move || loop {
                match stopped.recv_timeout(interval) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                let result = now_ms().and_then(|now| {
                    let expiry = now.checked_add(lease_duration_ms).ok_or_else(|| {
                        CheckpointError::new(
                            "checkpoint_claim_invalid",
                            "checkpoint lease overflow",
                        )
                    })?;
                    store
                        .renew_checkpoint_claim_v2(&key, &claim_token, expiry, now)
                        .and_then(|renewed| {
                            if renewed {
                                Ok(())
                            } else {
                                Err(CheckpointError::new(
                                    "checkpoint_lease_lost",
                                    "checkpoint heartbeat lost its claim",
                                ))
                            }
                        })
                });
                if let Err(failure) = result {
                    *error_for_thread
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(failure);
                    break;
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
        let Some(heartbeat) = &self.heartbeat else {
            return Ok(());
        };
        if let Some(error) = heartbeat
            .error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn reload(&mut self) -> CheckpointResult<()> {
        let key = self.checkpoint_key()?.to_string();
        self.checkpoint = self.store.load_checkpoint_v2(&key)?;
        if self.checkpoint.is_none() {
            return Err(CheckpointError::new(
                "checkpoint_not_found",
                "checkpoint disappeared from its store",
            ));
        }
        Ok(())
    }

    pub(super) fn require_checkpoint(&self) -> CheckpointResult<&CheckpointV2> {
        self.checkpoint.as_ref().ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_not_admitted",
                "checkpoint controller has not been admitted",
            )
        })
    }

    pub(super) fn require_checkpoint_mut(&mut self) -> CheckpointResult<&mut CheckpointV2> {
        self.checkpoint.as_mut().ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_not_admitted",
                "checkpoint controller has not been admitted",
            )
        })
    }
}
