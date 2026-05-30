use std::sync::Arc;

use crate::runtime::state::StateStore;
use crate::types::{CycleRecord, Message, Metadata};

pub(super) fn checkpoint_snapshot(
    state_store: &Arc<dyn StateStore>,
    task_id: &str,
) -> (Vec<Message>, Vec<CycleRecord>, Metadata) {
    match state_store.load_checkpoint(task_id) {
        Ok(Some(checkpoint)) => (
            checkpoint.messages,
            checkpoint.cycles,
            checkpoint.shared_state,
        ),
        Ok(None) | Err(_) => (Vec::new(), Vec::new(), Metadata::new()),
    }
}
