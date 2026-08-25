use super::*;

impl AppServerRunAdapter {
    /// Admit the narrow App Server controller surface through the durable
    /// checkpoint authority.  The client contributes only thread/turn/action
    /// identity and (for respond) a user message; all run fences and host
    /// interaction fields are loaded from the authoritative checkpoint.
    pub async fn controller_action(
        &self,
        params: TurnActionParams,
    ) -> Result<TurnActionResponse, AppServerError> {
        params.validate().map_err(AppServerError::invalid_params)?;
        let (store, checkpoint) = self
            .controller_binding(&params.thread_id, &params.turn_id)
            .await?;
        let command_id =
            derive_controller_command_id(&params.thread_id, &params.turn_id, &params.action_id)
                .map_err(|error| AppServerError::invalid_params(error.to_string()))?;

        if store
            .get_controller_command_receipt(&command_id)
            .map_err(|error| AppServerError::internal(error.to_string()))?
            .is_some()
        {
            let stored = store
                .get_controller_command(&command_id)
                .map_err(|error| AppServerError::internal(error.to_string()))?
                .ok_or_else(|| {
                    AppServerError::internal("controller action replay is missing its command")
                })?;
            if !same_public_action(&params.action, &stored.command) {
                return Err(AppServerError::invalid_params(
                    "actionId was reused with a different action payload",
                ));
            }
            let notification = public_notification_for_checkpoint(&*store, &checkpoint)?;
            let (status, wait_reason, _) =
                public_controller_status(&checkpoint, notification.as_ref());
            return Ok(TurnActionResponse {
                thread_id: params.thread_id,
                turn_id: params.turn_id,
                action_id: params.action_id,
                accepted: true,
                status,
                wait_reason,
            });
        }

        let command_variant = command_variant_from_action(&checkpoint, &params.action)?;
        let handle = ControllerHandle::new(
            checkpoint.checkpoint_key.clone(),
            checkpoint.root_run_id.clone(),
            checkpoint.trace_id.clone(),
        )
        .map_err(|error| AppServerError::internal(error.to_string()))?;
        let command = ControllerCommand::new(
            command_id,
            handle,
            checkpoint.resume_attempt,
            checkpoint.revision,
            command_variant,
        )
        .map_err(|error| AppServerError::invalid_params(error.to_string()))?;
        let resolution = store
            .resolve_controller_command(command)
            .map_err(|error| AppServerError::invalid_params(error.to_string()))?;
        let updated = store
            .load_checkpoint(&checkpoint.checkpoint_key)
            .map_err(|error| AppServerError::internal(error.to_string()))?
            .ok_or_else(|| {
                AppServerError::internal("checkpoint disappeared after controller action")
            })?;
        let notification = public_notification_for_checkpoint(&*store, &updated)?;
        let (status, wait_reason, _) = public_controller_status(&updated, notification.as_ref());
        Ok(TurnActionResponse {
            thread_id: params.thread_id,
            turn_id: params.turn_id,
            action_id: params.action_id,
            accepted: !matches!(resolution, ControllerCommandResolution::Rejected { .. }),
            status,
            wait_reason,
        })
    }

    pub async fn public_thread_status(
        &self,
        thread_id: &str,
    ) -> Result<ThreadStatusResponse, AppServerError> {
        let thread = self
            .store
            .get_thread(thread_id)
            .map_err(store_error)?
            .ok_or_else(AppServerError::thread_not_found)?;
        let turn = self
            .store
            .list_turns(thread_id)
            .map_err(store_error)?
            .into_iter()
            .rev()
            .find(|turn| {
                turn.status == TurnStatus::Running || turn.status == TurnStatus::Interrupted
            });
        let Some(turn) = turn else {
            return Ok(ThreadStatusResponse {
                thread_id: thread_id.to_string(),
                status: match thread.status {
                    ThreadStatus::Running => "running",
                    ThreadStatus::Closed | ThreadStatus::Archived => "idle",
                    ThreadStatus::Idle => "idle",
                    ThreadStatus::Interrupted => "interrupted",
                }
                .to_string(),
                wait_reason: None,
                prompt: None,
            });
        };
        let (store, checkpoint) = self.controller_binding(thread_id, &turn.turn_id).await?;
        let notification = public_notification_for_checkpoint(&*store, &checkpoint)?;
        let (status, wait_reason, prompt) =
            public_controller_status(&checkpoint, notification.as_ref());
        Ok(ThreadStatusResponse {
            thread_id: thread_id.to_string(),
            status,
            wait_reason,
            prompt,
        })
    }

    async fn controller_binding(
        &self,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<(Arc<dyn CheckpointStore>, Checkpoint), AppServerError> {
        let thread = self
            .store
            .get_thread(thread_id)
            .map_err(store_error)?
            .ok_or_else(AppServerError::thread_not_found)?;
        let turn = self
            .store
            .get_turn(thread_id, turn_id)
            .map_err(store_error)?
            .ok_or_else(|| AppServerError::invalid_params("Turn not found in thread"))?;
        let checkpoint_key = turn
            .result
            .get("checkpoint")
            .and_then(Value::as_object)
            .and_then(|checkpoint| checkpoint.get("key"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let active_store = self.state.active_turn(thread_id).await.and_then(|active| {
            active
                .checkpoint_store
                .map(|store| (store, checkpoint_key.clone()))
        });
        let (store, checkpoint_key) = if let Some((store, Some(key))) = active_store {
            (store, key)
        } else {
            let config = self
                .host
                .build_run_config(&RunConfigResolutionRequest {
                    thread_id: thread_id.to_string(),
                    agent_key: thread.agent_key.clone(),
                    cwd: thread.cwd.clone(),
                    metadata: thread.metadata.clone(),
                })
                .map_err(|error| AppServerError::internal(error.to_string()))?;
            let checkpoint = config.checkpoint_config.ok_or_else(|| {
                AppServerError::invalid_params("turn/action requires a retained durable checkpoint")
            })?;
            let store = checkpoint.store.ok_or_else(|| {
                AppServerError::invalid_params("turn/action requires a retained durable checkpoint")
            })?;
            let key = checkpoint_key.or(checkpoint.key).ok_or_else(|| {
                AppServerError::invalid_params("turn/action requires a checkpoint key")
            })?;
            (store, key)
        };
        let checkpoint = store
            .load_checkpoint(&checkpoint_key)
            .map_err(|error| AppServerError::internal(error.to_string()))?
            .ok_or_else(|| AppServerError::invalid_params("Checkpoint does not exist"))?;
        // A durable turn projection, when present, is the product-to-runtime
        // binding.  Active turns may not have persisted the summary yet, so
        // do not invent a task-id convention for host-defined AgentTask ids.
        if let Some(projected_key) = turn
            .result
            .get("checkpoint")
            .and_then(Value::as_object)
            .and_then(|value| value.get("key"))
            .and_then(Value::as_str)
        {
            if projected_key != checkpoint.checkpoint_key {
                return Err(AppServerError::invalid_params(
                    "Checkpoint does not belong to the requested turn",
                ));
            }
        }
        Ok((store, checkpoint))
    }
}

fn command_variant_from_action(
    checkpoint: &Checkpoint,
    action: &TurnAction,
) -> Result<ControllerCommandVariant, AppServerError> {
    match action {
        TurnAction::Respond { message } => {
            let request = match (
                checkpoint.status,
                checkpoint.active_host_interaction.clone(),
                checkpoint.suspended_origin.clone(),
            ) {
                (CheckpointStatus::HostInteraction, Some(request), _) => request,
                (CheckpointStatus::Suspended, _, Some(origin))
                    if origin.status == "host_interaction" =>
                {
                    origin.active_host_interaction.ok_or_else(|| {
                        AppServerError::invalid_params(
                            "respond requires a pending host interaction",
                        )
                    })?
                }
                _ => {
                    return Err(AppServerError::invalid_params(
                        "respond requires a pending host interaction",
                    ))
                }
            };
            let response = HostInteractionMessage::user(message.content.clone())
                .map_err(|error| AppServerError::invalid_params(error.to_string()))?;
            Ok(ControllerCommandVariant::HostInteractionResponse {
                interaction_id: request.interaction_id,
                logical_cycle: request.logical_cycle,
                operation_id: request.operation_id,
                tool_call_id: request.tool_call_id,
                request_digest: request.request_digest,
                response,
            })
        }
        TurnAction::Suspend => Ok(ControllerCommandVariant::Suspend),
        TurnAction::Resume => Ok(ControllerCommandVariant::Resume),
        TurnAction::Cancel => Ok(ControllerCommandVariant::Cancel),
        TurnAction::Abort => Ok(ControllerCommandVariant::Abort),
    }
}

fn same_public_action(action: &TurnAction, command: &ControllerCommandVariant) -> bool {
    match (action, command) {
        (
            TurnAction::Respond { message },
            ControllerCommandVariant::HostInteractionResponse { response, .. },
        ) => {
            // The durable command stores the canonical, redacted host message.
            // Normalize the replay input before comparing it; comparing the
            // raw client payload would reject a retry whose secret/locator was
            // removed during the first admission and could turn a safe replay
            // into an accidental second write.
            let Ok(incoming) = HostInteractionMessage::user(message.content.clone()) else {
                return false;
            };
            incoming.role == response.role && incoming.content == response.content
        }
        (TurnAction::Suspend, ControllerCommandVariant::Suspend)
        | (TurnAction::Resume, ControllerCommandVariant::Resume)
        | (TurnAction::Cancel, ControllerCommandVariant::Cancel)
        | (TurnAction::Abort, ControllerCommandVariant::Abort) => true,
        _ => false,
    }
}

fn public_notification_for_checkpoint(
    store: &dyn CheckpointStore,
    checkpoint: &Checkpoint,
) -> Result<Option<HostInteractionNotificationRecord>, AppServerError> {
    let request = checkpoint.active_host_interaction.clone().or_else(|| {
        checkpoint
            .suspended_origin
            .as_ref()
            .filter(|origin| origin.status == "host_interaction")
            .and_then(|origin| origin.active_host_interaction.clone())
    });
    let Some(request) = request else {
        return Ok(None);
    };
    let record_id = crate::checkpoint::record_id_for(&checkpoint.checkpoint_key, &request);
    let notification_id = crate::checkpoint::notification_id_for(&record_id);
    store
        .get_host_interaction_notification(&notification_id)
        .map_err(|error| AppServerError::internal(error.to_string()))
}

fn public_controller_status(
    checkpoint: &Checkpoint,
    notification: Option<&HostInteractionNotificationRecord>,
) -> (String, Option<String>, Option<String>) {
    match checkpoint.status {
        CheckpointStatus::HostInteraction => (
            "interrupted".to_string(),
            Some("host_interaction".to_string()),
            notification.map(|row| row.payload.prompt.clone()),
        ),
        CheckpointStatus::Suspended => (
            "interrupted".to_string(),
            Some("suspended".to_string()),
            // The suspended-origin record is recovery metadata, not an
            // active interaction.  Keep the immutable notification in the
            // durable store for a later resume, but never re-project its
            // prompt while the run is suspended.
            None,
        ),
        CheckpointStatus::Deferred => (
            "interrupted".to_string(),
            Some("deferred_pending".to_string()),
            None,
        ),
        CheckpointStatus::ReconciliationRequired => (
            "interrupted".to_string(),
            Some("reconciliation_required".to_string()),
            None,
        ),
        CheckpointStatus::Completed => ("completed".to_string(), None, None),
        CheckpointStatus::Failed => ("failed".to_string(), None, None),
        _ => ("running".to_string(), None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_server::protocol::TurnActionMessage;

    #[test]
    fn same_public_action_normalizes_replayed_host_message_before_compare() {
        let raw = "Use api_key=sk-test at https://example.test/path?q=secret.";
        let stored = HostInteractionMessage::user(raw).expect("sanitized message");
        let command = ControllerCommandVariant::HostInteractionResponse {
            interaction_id: "interaction-1".to_string(),
            logical_cycle: 1,
            operation_id: "operation-1".to_string(),
            tool_call_id: "tool-1".to_string(),
            request_digest: "a".repeat(64),
            response: stored,
        };
        let replay = TurnAction::Respond {
            message: TurnActionMessage {
                role: "user".to_string(),
                content: raw.to_string(),
            },
        };
        assert!(same_public_action(&replay, &command));
        let different = TurnAction::Respond {
            message: TurnActionMessage {
                role: "user".to_string(),
                content: "Use a different answer.".to_string(),
            },
        };
        assert!(!same_public_action(&different, &command));
    }
}
