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
            model_accounting: None,
        })
    }

    pub(crate) fn bind_model_accounting(&mut self, accounting: ModelCallCoordinator) {
        self.model_accounting = Some(accounting);
    }

    pub(crate) fn checkpoint_key(&self) -> CheckpointResult<&str> {
        Ok(&self.require_checkpoint()?.checkpoint_key)
    }

    pub(crate) fn checkpoint(&self) -> CheckpointResult<&Checkpoint> {
        self.require_checkpoint()
    }

    pub(crate) fn checkpoint_config(&self) -> &CheckpointConfig {
        &self.config
    }

    pub(crate) fn checkpoint_store(&self) -> Arc<dyn CheckpointStore> {
        self.store.clone()
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
}
