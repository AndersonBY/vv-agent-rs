use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use crate::events::{ModelCallFailureOutcome, RunEvent, RunEventPayload};
use crate::types::{ModelCallRecord, ModelCallStatus, TaskTokenUsage};

#[derive(Default)]
pub(super) struct ObservedRunProgress {
    completed_cycles: AtomicU32,
    token_usage: Mutex<Option<TaskTokenUsage>>,
}

impl ObservedRunProgress {
    pub(super) fn record_event(&self, event: &RunEvent) {
        let Some(cycle_index) = event.cycle_index().filter(|value| *value > 0) else {
            return;
        };
        let model_call = match event.payload() {
            RunEventPayload::ModelCallCompleted {
                call_id,
                operation_id,
                attempt,
                operation,
                backend,
                model,
                usage,
            } => {
                self.completed_cycles
                    .fetch_max(cycle_index, Ordering::Relaxed);
                Some(ModelCallRecord {
                    call_id: call_id.clone(),
                    operation_id: operation_id.clone(),
                    attempt: *attempt,
                    operation: *operation,
                    cycle_index,
                    backend: backend.clone(),
                    model: model.clone(),
                    status: ModelCallStatus::Completed,
                    usage: usage.clone(),
                    error_code: None,
                })
            }
            RunEventPayload::ModelCallFailed {
                call_id,
                operation_id,
                attempt,
                operation,
                backend,
                model,
                outcome,
                usage,
                error_code,
            } => Some(ModelCallRecord {
                call_id: call_id.clone(),
                operation_id: operation_id.clone(),
                attempt: *attempt,
                operation: *operation,
                cycle_index,
                backend: backend.clone(),
                model: model.clone(),
                status: match outcome {
                    ModelCallFailureOutcome::Definitive => ModelCallStatus::Failed,
                    ModelCallFailureOutcome::Ambiguous => ModelCallStatus::Ambiguous,
                },
                usage: usage.clone(),
                error_code: Some(error_code.clone()),
            }),
            _ => None,
        };
        let Some(model_call) = model_call else {
            return;
        };
        if let Ok(mut observed) = self.token_usage.lock() {
            let _ = observed
                .get_or_insert_with(TaskTokenUsage::default)
                .add_model_call(model_call);
        }
    }

    pub(super) fn token_usage(&self) -> Option<TaskTokenUsage> {
        self.token_usage.lock().ok().and_then(|usage| usage.clone())
    }

    pub(super) fn snapshot(&self) -> (u32, Option<TaskTokenUsage>) {
        (
            self.completed_cycles.load(Ordering::Relaxed),
            self.token_usage(),
        )
    }
}
