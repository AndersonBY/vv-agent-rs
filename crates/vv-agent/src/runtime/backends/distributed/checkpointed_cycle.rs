use crate::runtime::state::StateStore;
use crate::runtime::CancellationToken;
use crate::types::{AgentResult, AgentStatus, AgentTask, CycleRecord, Message, Metadata};

use super::CycleDispatchResult;

pub fn run_checkpointed_cycle<F>(
    state_store: &dyn StateStore,
    task: &AgentTask,
    cycle_index: u32,
    mut cycle_executor: F,
) -> Result<CycleDispatchResult, String>
where
    F: FnMut(
        u32,
        &mut Vec<Message>,
        &mut Vec<CycleRecord>,
        &mut Metadata,
        Option<&CancellationToken>,
    ) -> Option<AgentResult>,
{
    let Some(mut checkpoint) = state_store
        .load_checkpoint(&task.task_id)
        .map_err(|error| error.to_string())?
    else {
        return Ok(CycleDispatchResult::finished(AgentResult {
            status: AgentStatus::Failed,
            messages: Vec::new(),
            cycles: Vec::new(),
            final_answer: None,
            wait_reason: None,
            error: Some(format!("No checkpoint found for task {}", task.task_id)),
            shared_state: Metadata::new(),
            token_usage: Default::default(),
        }));
    };

    let result = cycle_executor(
        cycle_index,
        &mut checkpoint.messages,
        &mut checkpoint.cycles,
        &mut checkpoint.shared_state,
        None,
    );
    if let Some(result) = result {
        state_store
            .delete_checkpoint(&task.task_id)
            .map_err(|error| error.to_string())?;
        return Ok(CycleDispatchResult::finished(result));
    }

    checkpoint.cycle_index = cycle_index;
    checkpoint.status = AgentStatus::Running;
    state_store
        .save_checkpoint(checkpoint)
        .map_err(|error| error.to_string())?;
    Ok(CycleDispatchResult::unfinished())
}
