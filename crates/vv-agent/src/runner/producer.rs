use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use crate::agent::Agent;
use crate::result::RunResult;
use crate::run_config::RunConfig;
use crate::run_handle::{RunEventSenderSlot, RunHandle, RunHandleState, SharedRunResult};

use super::{CheckpointAdmissionSender, NormalizedInput, RunEventStream, Runner};

pub(crate) enum CheckpointStartOutcome {
    Started {
        handle: RunHandle,
        checkpoint: crate::runtime::state_v2::CheckpointV2,
    },
    ExistingOwner {
        checkpoint: crate::runtime::state_v2::CheckpointV2,
    },
    TerminalReplay {
        result: Box<RunResult>,
        checkpoint: crate::runtime::state_v2::CheckpointV2,
    },
}

impl Runner {
    pub async fn stream(
        &self,
        agent: &Agent,
        input: impl Into<NormalizedInput>,
    ) -> Result<RunEventStream, String> {
        self.stream_with_config(agent, input, RunConfig::default())
            .await
    }

    pub async fn stream_with_config(
        &self,
        agent: &Agent,
        input: impl Into<NormalizedInput>,
        config: RunConfig,
    ) -> Result<RunEventStream, String> {
        let handle = self.start(agent, input, config).await?;
        Ok(handle.into_event_stream())
    }

    pub async fn start(
        &self,
        agent: &Agent,
        input: impl Into<NormalizedInput>,
        config: RunConfig,
    ) -> Result<RunHandle, String> {
        self.start_internal(agent, input.into(), config, None).await
    }

    pub(crate) async fn start_checkpointed(
        &self,
        agent: &Agent,
        input: impl Into<NormalizedInput>,
        config: RunConfig,
    ) -> Result<CheckpointStartOutcome, String> {
        let checkpoint_config = config
            .checkpoint_config
            .clone()
            .or_else(|| self.default_run_config.checkpoint_config.clone())
            .ok_or_else(|| {
                "checkpoint_config_invalid: start_checkpointed requires checkpoint_config"
                    .to_string()
            })?;
        checkpoint_config
            .validate()
            .map_err(|error| error.to_string())?;
        let store = checkpoint_config.store.clone().ok_or_else(|| {
            "checkpoint_store_unavailable: start_checkpointed requires a process-local store"
                .to_string()
        })?;
        let checkpoint_key = checkpoint_config.key.clone().ok_or_else(|| {
            "checkpoint_key_required: start_checkpointed requires an explicit key".to_string()
        })?;
        if let Some(checkpoint) = store
            .load_checkpoint_v2(&checkpoint_key)
            .map_err(|error| error.to_string())?
        {
            if checkpoint.terminal_result.is_none()
                && checkpoint
                    .lease_expires_at_ms
                    .is_some_and(|expires_at| expires_at > unix_time_ms())
            {
                return Ok(CheckpointStartOutcome::ExistingOwner { checkpoint });
            }
        }

        let (admission_sender, admission_receiver) = tokio::sync::oneshot::channel();
        let handle = self
            .start_internal(agent, input.into(), config, Some(admission_sender))
            .await?;
        match admission_receiver.await {
            Ok(admission) if admission.terminal_replayed => {
                let result = handle.result().await?;
                let checkpoint = store
                    .load_checkpoint_v2(&checkpoint_key)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| {
                        "checkpoint_not_found: terminal checkpoint disappeared".to_string()
                    })?;
                Ok(CheckpointStartOutcome::TerminalReplay {
                    result: Box::new(result),
                    checkpoint,
                })
            }
            Ok(admission) => Ok(CheckpointStartOutcome::Started {
                handle,
                checkpoint: admission.checkpoint,
            }),
            Err(_) => {
                let result = handle.result().await;
                if let Some(checkpoint) = store
                    .load_checkpoint_v2(&checkpoint_key)
                    .map_err(|error| error.to_string())?
                {
                    if checkpoint.terminal_result.is_none()
                        && checkpoint
                            .lease_expires_at_ms
                            .is_some_and(|expires_at| expires_at > unix_time_ms())
                    {
                        return Ok(CheckpointStartOutcome::ExistingOwner { checkpoint });
                    }
                }
                match result {
                    Ok(_) => Err(
                        "checkpoint_admission_missing: checkpointed run completed without admission"
                            .to_string(),
                    ),
                    Err(error) => Err(error),
                }
            }
        }
    }

    async fn start_internal(
        &self,
        agent: &Agent,
        input: NormalizedInput,
        mut config: RunConfig,
        checkpoint_admission_sender: Option<CheckpointAdmissionSender>,
    ) -> Result<RunHandle, String> {
        let cancellation_token = config
            .cancellation_token
            .clone()
            .or_else(|| self.default_run_config.cancellation_token.clone())
            .unwrap_or_default();
        config.cancellation_token = Some(cancellation_token.clone());
        let approval_broker = config
            .approval_broker
            .clone()
            .or_else(|| self.default_run_config.approval_broker.clone())
            .unwrap_or_default();
        config.approval_broker = Some(approval_broker.clone());

        let (event_sender, _) = broadcast::channel(1024);
        let event_collector = Arc::new(Mutex::new(Vec::new()));
        let event_sender_slot: RunEventSenderSlot =
            Arc::new(Mutex::new(Some(event_sender.clone())));
        let state = Arc::new(Mutex::new(RunHandleState::running()));
        let cancel_requested = Arc::new(AtomicBool::new(false));
        let (completion_sender, completion_receiver) = tokio::sync::watch::channel(false);
        let runner = self.clone();
        let agent = agent.clone();
        let state_for_task = state.clone();
        let event_collector_for_task = event_collector.clone();
        let cancellation_token_for_task = cancellation_token.clone();
        let join = tokio::task::spawn_blocking(move || {
            struct CompletionGuard {
                sender: Option<tokio::sync::watch::Sender<bool>>,
            }

            impl Drop for CompletionGuard {
                fn drop(&mut self) {
                    if let Some(sender) = self.sender.take() {
                        let _ = sender.send(true);
                    }
                }
            }

            let _completion = CompletionGuard {
                sender: Some(completion_sender),
            };
            let result = runner.run_blocking_with_event_sender(
                &agent,
                input,
                config,
                Some(event_collector_for_task),
                Some(event_sender),
                checkpoint_admission_sender,
            );
            if let Ok(mut state) = state_for_task.lock() {
                *state = match &result {
                    Ok(result) if run_result_was_cancelled(result) => {
                        RunHandleState::cancelled_with_reason(
                            result
                                .result()
                                .error
                                .clone()
                                .unwrap_or_else(|| "Operation was cancelled".to_string()),
                        )
                    }
                    Ok(result) => RunHandleState::from_run_result(result),
                    Err(error)
                        if cancellation_token_for_task.is_cancelled()
                            && error.to_ascii_lowercase().contains("cancel") =>
                    {
                        let mut state = RunHandleState::cancelled();
                        state.error = Some(error.clone());
                        state
                    }
                    Err(error) => RunHandleState::failed(error.clone()),
                };
            }
            result
        });
        let result = SharedRunResult::new(join);
        Ok(RunHandle::new(
            event_sender_slot,
            event_collector,
            result,
            state,
            cancellation_token,
            approval_broker,
            completion_receiver,
            cancel_requested,
        ))
    }
}

fn unix_time_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn run_result_was_cancelled(result: &RunResult) -> bool {
    result.status() == crate::types::AgentStatus::Failed
        && result
            .result()
            .error
            .as_deref()
            .is_some_and(|error| error.to_ascii_lowercase().contains("cancel"))
}
