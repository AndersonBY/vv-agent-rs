use std::future::Future;
use std::pin::Pin;

use crate::app_server::protocol::{
    AppServerError, CheckpointSummaryStatus, TurnCompletedParams, TurnResumeResponse, TurnStatus,
};
use crate::checkpoint::MAX_WIRE_INTEGER;

pub type DurableTurnResumeFuture = Pin<
    Box<dyn Future<Output = Result<DurableTurnResumeOutcome, AppServerError>> + Send + 'static>,
>;

pub type DurableTurnCompletionFuture =
    Pin<Box<dyn Future<Output = Result<TurnCompletedParams, AppServerError>> + Send + 'static>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableTurnResumeRequest {
    pub thread_id: String,
    pub turn_id: String,
    pub checkpoint_key: String,
}

/// Narrow bridge from the App Server protocol to a real checkpoint-aware
/// Runner implementation. The bridge returns only protocol-safe projections;
/// raw checkpoint records never cross this boundary. `ExistingOwner` must use
/// the persisted checkpoint summary unchanged: the App Server never predicts
/// a status transition or increments `resume_attempt` on the owner's behalf.
pub trait DurableTurnResumeProvider: Send + Sync {
    fn resume_turn(&self, request: DurableTurnResumeRequest) -> DurableTurnResumeFuture;
}

pub enum DurableTurnResumeOutcome {
    Started {
        response: TurnResumeResponse,
        completion: DurableTurnCompletionFuture,
    },
    ExistingOwner {
        response: TurnResumeResponse,
    },
    TerminalReplay {
        response: TurnResumeResponse,
    },
}

impl DurableTurnResumeOutcome {
    pub fn response(&self) -> &TurnResumeResponse {
        match self {
            Self::Started { response, .. }
            | Self::ExistingOwner { response }
            | Self::TerminalReplay { response } => response,
        }
    }

    pub(crate) fn validate(
        &self,
        request: &DurableTurnResumeRequest,
    ) -> Result<(), AppServerError> {
        validate_response_identity(self.response(), request)?;
        match self {
            Self::Started { response, .. } => {
                validate_running_response(response, false)?;
            }
            Self::ExistingOwner { response } => {
                validate_running_response(response, true)?;
            }
            Self::TerminalReplay { response } => {
                if matches!(response.status, TurnStatus::Queued | TurnStatus::Running) {
                    return Err(invalid_bridge(
                        "terminal replay must return a terminal turn status",
                    ));
                }
                let checkpoint = response.checkpoint.as_ref().ok_or_else(|| {
                    invalid_bridge("terminal replay must include a checkpoint summary")
                })?;
                if matches!(
                    checkpoint.status,
                    CheckpointSummaryStatus::Pending
                        | CheckpointSummaryStatus::Running
                        | CheckpointSummaryStatus::ReconciliationRequired
                ) {
                    return Err(invalid_bridge(
                        "terminal replay checkpoint status is not terminal",
                    ));
                }
                validate_projection(
                    response.status,
                    response.completion_reason.as_deref(),
                    response.error.as_deref(),
                    response.checkpoint.as_ref(),
                    response.interruption.as_ref(),
                )?;
            }
        }
        Ok(())
    }
}

pub(crate) fn validate_completion(
    completion: &TurnCompletedParams,
    request: &DurableTurnResumeRequest,
    run_id: &str,
) -> Result<(), AppServerError> {
    if completion.thread_id != request.thread_id || completion.turn_id != request.turn_id {
        return Err(invalid_bridge(
            "durable completion changed the requested thread or turn",
        ));
    }
    if completion.run_id.as_deref() != Some(run_id) {
        return Err(invalid_bridge(
            "durable completion changed the resumed run id",
        ));
    }
    if matches!(completion.status, TurnStatus::Queued | TurnStatus::Running) {
        return Err(invalid_bridge(
            "durable completion must use a terminal turn status",
        ));
    }
    let checkpoint = completion
        .checkpoint
        .as_ref()
        .ok_or_else(|| invalid_bridge("durable completion must include a checkpoint summary"))?;
    validate_checkpoint(checkpoint, request)?;
    validate_projection(
        completion.status,
        completion.completion_reason.as_deref(),
        completion.error.as_deref(),
        completion.checkpoint.as_ref(),
        completion.interruption.as_ref(),
    )
}

fn validate_response_identity(
    response: &TurnResumeResponse,
    request: &DurableTurnResumeRequest,
) -> Result<(), AppServerError> {
    if response.thread_id != request.thread_id || response.turn_id != request.turn_id {
        return Err(invalid_bridge(
            "durable resume changed the requested thread or turn",
        ));
    }
    if response.run_id.is_empty() {
        return Err(invalid_bridge("durable resume run id must not be empty"));
    }
    if let Some(checkpoint) = &response.checkpoint {
        validate_checkpoint(checkpoint, request)?;
    }
    Ok(())
}

fn validate_checkpoint(
    checkpoint: &crate::app_server::protocol::CheckpointSummary,
    request: &DurableTurnResumeRequest,
) -> Result<(), AppServerError> {
    if checkpoint.key != request.checkpoint_key {
        return Err(invalid_bridge(
            "checkpoint summary key does not match checkpointKey",
        ));
    }
    if checkpoint.resume_attempt == 0 || checkpoint.resume_attempt > MAX_WIRE_INTEGER {
        return Err(invalid_bridge(
            "checkpoint summary resumeAttempt must be positive and JSON-safe",
        ));
    }
    if checkpoint.cycle_index > MAX_WIRE_INTEGER {
        return Err(invalid_bridge(
            "checkpoint summary cycleIndex must be JSON-safe",
        ));
    }
    Ok(())
}

fn validate_running_response(
    response: &TurnResumeResponse,
    checkpoint_required: bool,
) -> Result<(), AppServerError> {
    if response.status != TurnStatus::Running {
        return Err(invalid_bridge(
            "started or live-owner resume must return running",
        ));
    }
    if response.final_output.is_some()
        || response.completion_reason.is_some()
        || response.completion_tool_name.is_some()
        || response.partial_output.is_some()
        || response.interruption.is_some()
        || response.error.is_some()
    {
        return Err(invalid_bridge(
            "running resume response contains terminal fields",
        ));
    }
    if checkpoint_required && response.checkpoint.is_none() {
        return Err(invalid_bridge(
            "live-owner response must include a checkpoint summary",
        ));
    }
    if let Some(checkpoint) = &response.checkpoint {
        if checkpoint.status != CheckpointSummaryStatus::Running {
            return Err(invalid_bridge(
                "running response checkpoint status must be running",
            ));
        }
        if checkpoint.terminal_acknowledged {
            return Err(invalid_bridge(
                "running response checkpoint cannot be terminal-acknowledged",
            ));
        }
    }
    Ok(())
}

fn validate_projection(
    status: TurnStatus,
    completion_reason: Option<&str>,
    error: Option<&str>,
    checkpoint: Option<&crate::app_server::protocol::CheckpointSummary>,
    interruption: Option<&crate::app_server::protocol::InterruptionSummary>,
) -> Result<(), AppServerError> {
    let checkpoint = checkpoint
        .ok_or_else(|| invalid_bridge("terminal projection must include a checkpoint summary"))?;
    let reconciliation = checkpoint.status == CheckpointSummaryStatus::ReconciliationRequired;
    let expected_status = match checkpoint.status {
        CheckpointSummaryStatus::Completed => TurnStatus::Completed,
        CheckpointSummaryStatus::Failed | CheckpointSummaryStatus::MaxCycles => TurnStatus::Failed,
        CheckpointSummaryStatus::WaitUser | CheckpointSummaryStatus::ReconciliationRequired => {
            TurnStatus::Interrupted
        }
        CheckpointSummaryStatus::Pending | CheckpointSummaryStatus::Running => {
            return Err(invalid_bridge(
                "terminal projection contains a non-terminal checkpoint status",
            ));
        }
    };
    if status != expected_status {
        return Err(invalid_bridge(
            "checkpoint status and projected turn status disagree",
        ));
    }
    if reconciliation {
        if completion_reason.is_some() || error.is_some() {
            return Err(invalid_bridge(
                "reconciliation_required must omit completionReason and error",
            ));
        }
        let interruption = interruption.ok_or_else(|| {
            invalid_bridge("reconciliation_required must include interruption details")
        })?;
        if interruption.reason != "resume_requires_reconciliation" {
            return Err(invalid_bridge(
                "reconciliation interruption reason is not canonical",
            ));
        }
        if interruption.operation_id.is_empty()
            || interruption.cycle_index == 0
            || interruption.cycle_index > MAX_WIRE_INTEGER
            || interruption.risk.is_empty()
        {
            return Err(invalid_bridge(
                "reconciliation interruption details are incomplete",
            ));
        }
    } else if interruption.is_some() {
        return Err(invalid_bridge(
            "interruption details require reconciliation_required checkpoint status",
        ));
    }
    Ok(())
}

fn invalid_bridge(message: impl Into<String>) -> AppServerError {
    AppServerError::internal(format!(
        "invalid durable turn resume provider result: {}",
        message.into()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_server::protocol::{
        CheckpointSummary, InterruptionIdempotencySupport, InterruptionOperationKind,
        InterruptionSummary,
    };

    #[test]
    fn terminal_projection_rejects_running_checkpoint() {
        let request = request();
        let completion = completion(
            TurnStatus::Completed,
            CheckpointSummaryStatus::Running,
            None,
            None,
        );

        let error = validate_completion(&completion, &request, "run-1")
            .expect_err("running checkpoint cannot be terminal");

        assert!(error.message().contains("non-terminal checkpoint status"));
    }

    #[test]
    fn reconciliation_projection_rejects_completion_reason_and_error() {
        let request = request();
        let completion = completion(
            TurnStatus::Interrupted,
            CheckpointSummaryStatus::ReconciliationRequired,
            Some("failed"),
            Some("not definitive"),
        );

        let error = validate_completion(&completion, &request, "run-1")
            .expect_err("reconciliation is not failure or completion");

        assert!(error
            .message()
            .contains("must omit completionReason and error"));
    }

    fn request() -> DurableTurnResumeRequest {
        DurableTurnResumeRequest {
            thread_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
            checkpoint_key: "checkpoint-1".to_string(),
        }
    }

    fn completion(
        status: TurnStatus,
        checkpoint_status: CheckpointSummaryStatus,
        completion_reason: Option<&str>,
        error: Option<&str>,
    ) -> TurnCompletedParams {
        TurnCompletedParams {
            thread_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
            run_id: Some("run-1".to_string()),
            status,
            final_output: None,
            completion_reason: completion_reason.map(str::to_string),
            completion_tool_name: None,
            partial_output: None,
            error: error.map(str::to_string),
            token_usage: None,
            budget_usage: None,
            budget_exhaustion: None,
            checkpoint: Some(CheckpointSummary {
                key: "checkpoint-1".to_string(),
                resume_attempt: 2,
                cycle_index: 1,
                status: checkpoint_status,
                terminal_acknowledged: false,
            }),
            interruption: (checkpoint_status == CheckpointSummaryStatus::ReconciliationRequired)
                .then(|| InterruptionSummary {
                    reason: "resume_requires_reconciliation".to_string(),
                    operation_id: "operation-1".to_string(),
                    operation_kind: InterruptionOperationKind::Tool,
                    cycle_index: 2,
                    risk: "unknown_tool_side_effect".to_string(),
                    idempotency_support: InterruptionIdempotencySupport::Unknown,
                }),
        }
    }
}
