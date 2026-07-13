use std::sync::Arc;

use crate::runtime::state::{Checkpoint, StateStore};
use crate::types::{AgentResult, CycleRecord, Message, Metadata};

pub(super) fn load_checkpoint(
    state_store: &Arc<dyn StateStore>,
    task_id: &str,
    operation: &str,
) -> Result<Option<Checkpoint>, String> {
    state_store
        .load_checkpoint(task_id)
        .map_err(|error| format!("{operation}: failed to load checkpoint: {error}"))
}

pub(super) fn checkpoint_snapshot(
    checkpoint: &Checkpoint,
) -> (Vec<Message>, Vec<CycleRecord>, Metadata) {
    (
        checkpoint.messages.clone(),
        checkpoint.cycles.clone(),
        checkpoint.shared_state.clone(),
    )
}

pub(super) fn terminal_checkpoint(checkpoint: &Checkpoint, result: &AgentResult) -> Checkpoint {
    Checkpoint {
        task_id: checkpoint.task_id.clone(),
        cycle_index: checkpoint.cycle_index,
        status: result.status,
        messages: result.messages.clone(),
        cycles: result.cycles.clone(),
        shared_state: result.shared_state.clone(),
        revision: checkpoint.revision,
        claim_token: None,
        claimed_cycle: None,
        lease_expires_at_ms: None,
        terminal_result: Some(result.clone()),
    }
}
