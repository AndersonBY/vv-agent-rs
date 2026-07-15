use crate::runtime::state::StateStore;
use crate::runtime::CancellationToken;
use crate::types::{
    AgentResult, AgentStatus, AgentTask, CompletionReason, CycleRecord, Message, Metadata,
};

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
    if let Some(checkpoint) = state_store
        .load_checkpoint(&task.task_id)
        .map_err(|error| error.to_string())?
    {
        if let Some(result) = checkpoint.terminal_result {
            return Ok(CycleDispatchResult::finished_at_revision(
                result,
                Some(checkpoint.revision),
            ));
        }
    }
    let now_ms: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_millis()
        .try_into()
        .map_err(|_| "system clock milliseconds exceed u64".to_string())?;
    let lease_expires_at_ms = now_ms
        .checked_add(5 * 60 * 1000)
        .ok_or_else(|| "checkpoint lease overflow".to_string())?;
    let claim_token = uuid::Uuid::new_v4().simple().to_string();
    let Some(mut checkpoint) = state_store
        .claim_checkpoint(
            &task.task_id,
            cycle_index,
            &claim_token,
            lease_expires_at_ms,
            now_ms,
        )
        .map_err(|error| error.to_string())?
    else {
        return Ok(CycleDispatchResult::finished(AgentResult {
            status: AgentStatus::Failed,
            messages: Vec::new(),
            cycles: Vec::new(),
            completion_reason: Some(CompletionReason::Failed),
            completion_tool_name: None,
            partial_output: None,
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
        checkpoint.cycle_index = cycle_index;
        checkpoint.status = result.status;
        checkpoint.messages = result.messages.clone();
        checkpoint.cycles = result.cycles.clone();
        checkpoint.shared_state = result.shared_state.clone();
        checkpoint.terminal_result = Some(result.clone());
        let expected_revision = checkpoint.revision;
        if !state_store
            .commit_checkpoint(checkpoint, &claim_token, expected_revision)
            .map_err(|error| error.to_string())?
        {
            return Err(format!(
                "checkpoint changed while terminal cycle {cycle_index} was running for task {}",
                task.task_id
            ));
        }
        return Ok(CycleDispatchResult::finished_at_revision(
            result,
            Some(expected_revision + 1),
        ));
    }

    checkpoint.cycle_index = cycle_index;
    checkpoint.status = AgentStatus::Running;
    let expected_revision = checkpoint.revision;
    if !state_store
        .commit_checkpoint(checkpoint, &claim_token, expected_revision)
        .map_err(|error| error.to_string())?
    {
        return Err(format!(
            "checkpoint changed while cycle {cycle_index} was running for task {}",
            task.task_id
        ));
    }
    Ok(CycleDispatchResult::unfinished())
}
