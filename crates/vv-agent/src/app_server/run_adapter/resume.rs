use super::*;

impl AppServerRunAdapter {
    async fn resume_turn_with_runner(
        &self,
        thread: &AppThread,
        turn: &AppTurn,
        request: DurableTurnResumeRequest,
    ) -> Result<DurableTurnResumeOutcome, AppServerError> {
        let base_config_request = RunConfigResolutionRequest {
            thread_id: thread.thread_id.clone(),
            agent_key: thread.agent_key.clone(),
            cwd: thread.cwd.clone(),
            metadata: thread.metadata.clone(),
        };
        let base_config = self
            .host
            .build_run_config(&base_config_request)
            .map_err(|error| AppServerError::internal(error.to_string()))?;
        let base_checkpoint_config = base_config.checkpoint_config.clone().ok_or_else(|| {
            AppServerError::invalid_params(
                "turn/resume requires the App Server host to configure checkpoint v2",
            )
        })?;
        let checkpoint_store = base_checkpoint_config.store.clone().ok_or_else(|| {
            AppServerError::invalid_params(
                "the local App Server Runner requires a process-local checkpoint store",
            )
        })?;
        let checkpoint = checkpoint_store
            .load_checkpoint(&request.checkpoint_key)
            .map_err(|error| AppServerError::internal(error.to_string()))?
            .ok_or_else(|| AppServerError::invalid_params("Checkpoint not found"))?;

        let mut effective_metadata = thread.metadata.clone();
        if let Some(run_metadata) = checkpoint
            .run_definition
            .get("run_metadata")
            .and_then(Value::as_object)
        {
            effective_metadata.extend(
                run_metadata
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
        }
        let agent_request = AgentResolutionRequest {
            thread_id: thread.thread_id.clone(),
            agent_key: thread.agent_key.clone(),
            cwd: thread.cwd.clone(),
            metadata: effective_metadata.clone(),
        };
        let config_request = RunConfigResolutionRequest {
            thread_id: thread.thread_id.clone(),
            agent_key: thread.agent_key.clone(),
            cwd: thread.cwd.clone(),
            metadata: effective_metadata.clone(),
        };
        let agent = self
            .host
            .resolve_agent(&agent_request)
            .map_err(|error| AppServerError::internal(error.to_string()))?;
        let mut config = self
            .host
            .build_run_config(&config_request)
            .map_err(|error| AppServerError::internal(error.to_string()))?;
        let mut checkpoint_config = config
            .checkpoint_config
            .take()
            .unwrap_or(base_checkpoint_config);
        checkpoint_config.store = Some(checkpoint_store.clone());
        checkpoint_config.store_ref = None;
        checkpoint_config.key = Some(request.checkpoint_key.clone());
        checkpoint_config.resume_policy = ResumePolicy::RequireExisting;
        config.checkpoint_config = Some(checkpoint_config);
        if config.approval_provider.is_none() {
            config.approval_provider = Some(Arc::new(AppServerApprovalProvider));
        }
        if config.approval_broker.is_none() {
            config.approval_broker = Some(ApprovalBroker::default());
        }
        config.hooks.push(Arc::new(SteeringRuntimeHook {
            queue: SteeringQueue::default(),
        }));
        config.metadata.extend(effective_metadata);
        config
            .metadata
            .insert("thread_id".to_string(), json!(request.thread_id));
        config
            .metadata
            .insert("turn_id".to_string(), json!(request.turn_id));
        config
            .metadata
            .insert("session_id".to_string(), json!(request.thread_id));

        match self
            .runner
            .start_checkpointed(&agent, input_text(&turn.input), config)
            .await
            .map_err(AppServerError::internal)?
        {
            CheckpointStartOutcome::ExistingOwner { checkpoint } => {
                Ok(DurableTurnResumeOutcome::ExistingOwner {
                    response: running_resume_response(&request, &checkpoint, true),
                })
            }
            CheckpointStartOutcome::TerminalReplay { result, checkpoint } => {
                Ok(DurableTurnResumeOutcome::TerminalReplay {
                    response: resume_response_from_result(&request, result.result(), &checkpoint),
                })
            }
            CheckpointStartOutcome::Started { handle, checkpoint } => {
                let response = running_resume_response(&request, &checkpoint, false);
                let completion_request = request.clone();
                let completion_store = checkpoint_store;
                let completion: DurableTurnCompletionFuture = Box::pin(async move {
                    let run_result = handle.result().await;
                    let checkpoint = completion_store
                        .load_checkpoint(&completion_request.checkpoint_key)
                        .map_err(|error| AppServerError::internal(error.to_string()))?
                        .ok_or_else(|| {
                            AppServerError::internal(
                                "checkpoint disappeared after durable resume admission",
                            )
                        })?;
                    let result = match run_result {
                        Ok(result) => result.result().clone(),
                        Err(error) => checkpoint
                            .terminal_result
                            .as_ref()
                            .map(crate::types::AgentResult::from_dict)
                            .transpose()
                            .map_err(AppServerError::internal)?
                            .ok_or_else(|| AppServerError::internal(error))?,
                    };
                    Ok(completion_from_agent_result(
                        &completion_request,
                        &result,
                        &checkpoint,
                    ))
                });
                Ok(DurableTurnResumeOutcome::Started {
                    response,
                    completion,
                })
            }
        }
    }

    pub(crate) async fn prepare_turn_resume(
        &self,
        params: TurnResumeParams,
    ) -> Result<PreparedTurnResume, AppServerError> {
        if params.thread_id.is_empty()
            || params.turn_id.is_empty()
            || params.checkpoint_key.is_empty()
        {
            return Err(AppServerError::invalid_params(
                "threadId, turnId, and checkpointKey must not be empty",
            ));
        }
        if params.checkpoint_key.len() > MAX_CHECKPOINT_KEY_BYTES {
            return Err(AppServerError::invalid_params(
                "checkpointKey exceeds the 512-byte checkpoint limit",
            ));
        }
        let thread = self
            .store
            .get_thread(&params.thread_id)
            .map_err(store_error)?
            .ok_or_else(AppServerError::thread_not_found)?;
        if thread.archived_at.is_some() {
            return Err(AppServerError::thread_archived());
        }
        let turn = self
            .store
            .get_turn(&params.thread_id, &params.turn_id)
            .map_err(store_error)?
            .ok_or_else(|| AppServerError::invalid_params("Turn not found in thread"))?;

        let resume_request = DurableTurnResumeRequest {
            thread_id: params.thread_id,
            turn_id: params.turn_id,
            checkpoint_key: params.checkpoint_key,
        };
        let outcome = if let Some(provider) = self.durable_resume_provider.as_ref() {
            provider.resume_turn(resume_request.clone()).await?
        } else {
            self.resume_turn_with_runner(&thread, &turn, resume_request.clone())
                .await?
        };
        outcome.validate(&resume_request)?;
        let response = outcome.response().clone();

        match outcome {
            DurableTurnResumeOutcome::Started { completion, .. } => {
                if self.state.has_active_turn(&resume_request.thread_id).await {
                    return Err(AppServerError::internal(
                        "durable resume provider started a second local owner",
                    ));
                }
                self.persist_running_resume(&resume_request, &response.run_id)?;
                self.state
                    .set_durable_resume(
                        resume_request.thread_id.clone(),
                        resume_request.turn_id.clone(),
                    )
                    .await;
                Ok(PreparedTurnResume {
                    response,
                    continuation: Some(PreparedTurnResumeContinuation {
                        request: resume_request,
                        completion,
                    }),
                })
            }
            DurableTurnResumeOutcome::ExistingOwner { .. } => {
                self.persist_running_resume(&resume_request, &response.run_id)?;
                Ok(PreparedTurnResume {
                    response,
                    continuation: None,
                })
            }
            DurableTurnResumeOutcome::TerminalReplay { .. } => {
                let completion = completion_from_resume_response(&response);
                self.persist_terminal_replay(&completion)?;
                self.state
                    .clear_durable_resume(&resume_request.thread_id, &resume_request.turn_id)
                    .await;
                Ok(PreparedTurnResume {
                    response,
                    continuation: None,
                })
            }
        }
    }

    pub(crate) async fn dispatch_turn_resume(&self, prepared: PreparedTurnResume) {
        let Some(continuation) = prepared.continuation else {
            return;
        };
        let run_id = prepared.response.run_id;
        let _ = self
            .broadcast_to_thread(
                &continuation.request.thread_id,
                ServerNotification::ThreadStatusChanged(ThreadStatusChangedParams {
                    thread_id: continuation.request.thread_id.clone(),
                    status: ThreadStatus::Running,
                }),
            )
            .await;
        let _ = self
            .broadcast_to_thread(
                &continuation.request.thread_id,
                ServerNotification::TurnStarted(TurnStartedParams {
                    thread_id: continuation.request.thread_id.clone(),
                    turn_id: continuation.request.turn_id.clone(),
                    run_id: Some(run_id.clone()),
                    status: Some(TurnStatus::Running),
                }),
            )
            .await;

        let adapter = self.clone();
        tokio::spawn(async move {
            let completion = match continuation.completion.await {
                Ok(completion) => completion,
                Err(error) => {
                    adapter
                        .state
                        .clear_durable_resume(
                            &continuation.request.thread_id,
                            &continuation.request.turn_id,
                        )
                        .await;
                    let _ = adapter
                        .broadcast_to_thread(
                            &continuation.request.thread_id,
                            ServerNotification::ErrorWarning(
                                crate::app_server::protocol::WarningParams {
                                    message: error.message().to_string(),
                                    code: Some("durable_resume_runtime".to_string()),
                                },
                            ),
                        )
                        .await;
                    return;
                }
            };
            if let Err(error) = validate_completion(&completion, &continuation.request, &run_id) {
                let _ = adapter
                    .broadcast_to_thread(
                        &continuation.request.thread_id,
                        ServerNotification::ErrorWarning(
                            crate::app_server::protocol::WarningParams {
                                message: error.message().to_string(),
                                code: Some("durable_resume_projection".to_string()),
                            },
                        ),
                    )
                    .await;
                return;
            }
            adapter
                .complete_durable_resume(continuation.request, completion)
                .await;
        });
    }

    fn persist_running_resume(
        &self,
        request: &DurableTurnResumeRequest,
        run_id: &str,
    ) -> Result<(), AppServerError> {
        let turn = self
            .store
            .get_turn(&request.thread_id, &request.turn_id)
            .map_err(store_error)?
            .ok_or_else(|| AppServerError::invalid_params("Turn not found in thread"))?;
        if turn.status != TurnStatus::Running || turn.run_id.as_deref() != Some(run_id) {
            self.store
                .mark_turn_running(&request.thread_id, &request.turn_id, run_id)
                .map_err(store_error)?;
        }
        let thread = self
            .store
            .get_thread(&request.thread_id)
            .map_err(store_error)?
            .ok_or_else(AppServerError::thread_not_found)?;
        if thread.status != ThreadStatus::Running {
            self.store
                .set_active_turn(
                    &request.thread_id,
                    Some(&request.turn_id),
                    ThreadStatus::Running,
                )
                .map_err(store_error)?;
        }
        Ok(())
    }

    fn persist_terminal_replay(
        &self,
        completion: &TurnCompletedParams,
    ) -> Result<(), AppServerError> {
        let turn = self
            .store
            .get_turn(&completion.thread_id, &completion.turn_id)
            .map_err(store_error)?
            .ok_or_else(|| AppServerError::invalid_params("Turn not found in thread"))?;
        let mut expected_result = turn_completion_result(completion);
        for field in ["tokenUsage", "budgetUsage", "budgetExhaustion"] {
            if !expected_result.contains_key(field) {
                if let Some(value) = turn.result.get(field) {
                    expected_result.insert(field.to_string(), value.clone());
                }
            }
        }
        if turn.status != completion.status
            || turn.run_id.as_deref() != completion.run_id.as_deref()
            || turn.result != expected_result
        {
            self.persist_turn_completion(completion)?;
        }
        let thread = self
            .store
            .get_thread(&completion.thread_id)
            .map_err(store_error)?
            .ok_or_else(AppServerError::thread_not_found)?;
        if thread.status != ThreadStatus::Idle {
            self.store
                .set_active_turn(&completion.thread_id, None, ThreadStatus::Idle)
                .map_err(store_error)?;
        }
        Ok(())
    }

    async fn complete_durable_resume(
        &self,
        request: DurableTurnResumeRequest,
        completion: TurnCompletedParams,
    ) {
        if let Err(error) = self.persist_turn_completion(&completion) {
            let _ = self
                .broadcast_to_thread(
                    &request.thread_id,
                    ServerNotification::ErrorWarning(crate::app_server::protocol::WarningParams {
                        message: error.message().to_string(),
                        code: Some("durable_resume_store".to_string()),
                    }),
                )
                .await;
            return;
        }
        self.state
            .clear_durable_resume(&request.thread_id, &request.turn_id)
            .await;
        let _ = self
            .store
            .set_active_turn(&request.thread_id, None, ThreadStatus::Idle);
        let _ = self
            .broadcast_to_thread(
                &request.thread_id,
                ServerNotification::ThreadStatusChanged(ThreadStatusChangedParams {
                    thread_id: request.thread_id.clone(),
                    status: ThreadStatus::Idle,
                }),
            )
            .await;
        let _ = self
            .broadcast_to_thread(
                &request.thread_id,
                ServerNotification::TurnCompleted(completion),
            )
            .await;
    }
}

fn checkpoint_summary(checkpoint: &Checkpoint) -> CheckpointSummary {
    CheckpointSummary {
        key: checkpoint.checkpoint_key.clone(),
        resume_attempt: checkpoint.resume_attempt,
        cycle_index: checkpoint.cycle_index,
        status: match checkpoint.status {
            CheckpointStatus::Pending => CheckpointSummaryStatus::Pending,
            CheckpointStatus::Running => CheckpointSummaryStatus::Running,
            CheckpointStatus::WaitUser => CheckpointSummaryStatus::WaitUser,
            CheckpointStatus::Completed => CheckpointSummaryStatus::Completed,
            CheckpointStatus::Failed => CheckpointSummaryStatus::Failed,
            CheckpointStatus::MaxCycles => CheckpointSummaryStatus::MaxCycles,
            CheckpointStatus::ReconciliationRequired => {
                CheckpointSummaryStatus::ReconciliationRequired
            }
        },
        terminal_acknowledged: checkpoint.terminal_acknowledged,
    }
}

pub(super) fn checkpoint_projection(
    result: &RunResult,
    store: Option<&Arc<dyn CheckpointStore>>,
) -> Result<(Option<CheckpointSummary>, Option<InterruptionSummary>), String> {
    let Some(checkpoint_key) = result.checkpoint_key() else {
        return Ok((None, None));
    };
    let store = store.ok_or_else(|| {
        "checkpoint_store_unavailable: App Server turn lost its checkpoint store".to_string()
    })?;
    let checkpoint = store
        .load_checkpoint(checkpoint_key)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            format!(
                "checkpoint_not_found: App Server turn checkpoint {checkpoint_key:?} disappeared"
            )
        })?;
    if checkpoint.root_run_id != result.run_id() {
        return Err(
            "checkpoint_identity_mismatch: App Server checkpoint run id changed".to_string(),
        );
    }
    Ok((
        Some(checkpoint_summary(&checkpoint)),
        result.resume_observation().map(interruption_summary),
    ))
}

fn interruption_summary(observation: &ResumeObservation) -> InterruptionSummary {
    InterruptionSummary {
        reason: "resume_requires_reconciliation".to_string(),
        operation_id: observation.operation_id.clone(),
        operation_kind: match observation.operation_kind {
            OperationKind::Model => InterruptionOperationKind::Model,
            OperationKind::Tool => InterruptionOperationKind::Tool,
        },
        cycle_index: observation.cycle_index,
        risk: observation.risk.clone(),
        idempotency_support: match observation.idempotency_support {
            Some(ToolIdempotency::Supported) => InterruptionIdempotencySupport::Supported,
            Some(ToolIdempotency::Unsupported) => InterruptionIdempotencySupport::Unsupported,
            Some(ToolIdempotency::Unknown) | None => InterruptionIdempotencySupport::Unknown,
        },
    }
}

fn running_resume_response(
    request: &DurableTurnResumeRequest,
    checkpoint: &Checkpoint,
    include_checkpoint: bool,
) -> TurnResumeResponse {
    TurnResumeResponse {
        thread_id: request.thread_id.clone(),
        turn_id: request.turn_id.clone(),
        run_id: checkpoint.root_run_id.clone(),
        status: TurnStatus::Running,
        final_output: None,
        completion_reason: None,
        completion_tool_name: None,
        partial_output: None,
        checkpoint: include_checkpoint.then(|| checkpoint_summary(checkpoint)),
        interruption: None,
        error: None,
    }
}

fn resume_response_from_result(
    request: &DurableTurnResumeRequest,
    result: &crate::types::AgentResult,
    checkpoint: &Checkpoint,
) -> TurnResumeResponse {
    TurnResumeResponse {
        thread_id: request.thread_id.clone(),
        turn_id: request.turn_id.clone(),
        run_id: checkpoint.root_run_id.clone(),
        status: turn_status(result.status),
        final_output: result
            .final_answer
            .clone()
            .or_else(|| result.wait_reason.clone())
            .or_else(|| result.error.clone()),
        completion_reason: result
            .completion_reason
            .map(|reason| reason.as_str().to_string()),
        completion_tool_name: result.completion_tool_name.clone(),
        partial_output: result.partial_output.clone(),
        checkpoint: Some(checkpoint_summary(checkpoint)),
        interruption: result.resume_observation.as_ref().map(interruption_summary),
        error: (result.status == AgentStatus::Failed)
            .then(|| result.error.clone())
            .flatten(),
    }
}

fn completion_from_agent_result(
    request: &DurableTurnResumeRequest,
    result: &crate::types::AgentResult,
    checkpoint: &Checkpoint,
) -> TurnCompletedParams {
    let response = resume_response_from_result(request, result, checkpoint);
    TurnCompletedParams {
        thread_id: response.thread_id,
        turn_id: response.turn_id,
        run_id: Some(response.run_id),
        status: response.status,
        final_output: response.final_output,
        completion_reason: response.completion_reason,
        completion_tool_name: response.completion_tool_name,
        partial_output: response.partial_output,
        error: response.error,
        token_usage: Some(app_token_usage(&result.token_usage)),
        budget_usage: result.budget_usage.as_ref().map(app_json_object),
        budget_exhaustion: result.budget_exhaustion.as_ref().map(app_json_object),
        checkpoint: response.checkpoint,
        interruption: response.interruption,
    }
}

fn completion_from_resume_response(response: &TurnResumeResponse) -> TurnCompletedParams {
    TurnCompletedParams {
        thread_id: response.thread_id.clone(),
        turn_id: response.turn_id.clone(),
        run_id: Some(response.run_id.clone()),
        status: response.status,
        final_output: response.final_output.clone(),
        completion_reason: response.completion_reason.clone(),
        completion_tool_name: response.completion_tool_name.clone(),
        partial_output: response.partial_output.clone(),
        error: response.error.clone(),
        token_usage: None,
        budget_usage: None,
        budget_exhaustion: None,
        checkpoint: response.checkpoint.clone(),
        interruption: response.interruption.clone(),
    }
}

pub(super) fn turn_completion_result(completion: &TurnCompletedParams) -> BTreeMap<String, Value> {
    let mut result = BTreeMap::new();
    if let Some(value) = &completion.final_output {
        result.insert("finalOutput".to_string(), json!(value));
    }
    if let Some(value) = &completion.completion_reason {
        result.insert("completionReason".to_string(), json!(value));
    }
    if let Some(value) = &completion.completion_tool_name {
        result.insert("completionToolName".to_string(), json!(value));
    }
    if let Some(value) = &completion.partial_output {
        result.insert("partialOutput".to_string(), json!(value));
    }
    if let Some(value) = &completion.error {
        result.insert("error".to_string(), json!(value));
    }
    if let Some(value) = &completion.token_usage {
        result.insert("tokenUsage".to_string(), json!(value));
    }
    if let Some(value) = &completion.budget_usage {
        result.insert("budgetUsage".to_string(), json!(value));
    }
    if let Some(value) = &completion.budget_exhaustion {
        result.insert("budgetExhaustion".to_string(), json!(value));
    }
    if let Some(value) = &completion.checkpoint {
        result.insert("checkpoint".to_string(), json!(value));
    }
    if let Some(value) = &completion.interruption {
        result.insert("interruption".to_string(), json!(value));
    }
    result
}
