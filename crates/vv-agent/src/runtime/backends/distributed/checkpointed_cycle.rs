use crate::runtime::state::StateStore;
use crate::runtime::CancellationToken;
use crate::types::{
    AgentResult, AgentStatus, AgentTask, CompletionReason, CycleRecord, Message, Metadata,
};

use super::contract::{now_unix_ms, DEFAULT_LEASE_DURATION_MS};
use super::worker::{lease_expiry_at, run_with_checkpoint_lease, LeaseOperationResult};
use super::CycleDispatchResult;

pub fn run_checkpointed_cycle<F>(
    state_store: &dyn StateStore,
    task: &AgentTask,
    cycle_index: u32,
    cycle_executor: F,
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
    run_checkpointed_cycle_with_lease(
        state_store,
        task,
        cycle_index,
        DEFAULT_LEASE_DURATION_MS,
        cycle_executor,
    )
}

fn run_checkpointed_cycle_with_lease<F>(
    state_store: &dyn StateStore,
    task: &AgentTask,
    cycle_index: u32,
    lease_duration_ms: u64,
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
    let now_ms = now_unix_ms()?;
    let lease_expires_at_ms = lease_expiry_at(now_ms, lease_duration_ms, None)?;
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

    let expected_revision = checkpoint.revision;
    run_with_checkpoint_lease(
        state_store,
        &task.task_id,
        cycle_index,
        lease_duration_ms,
        None,
        &claim_token,
        expected_revision,
        |heartbeat_status| {
            let cycle_result = (|| -> Result<CycleDispatchResult, String> {
                let result = cycle_executor(
                    cycle_index,
                    &mut checkpoint.messages,
                    &mut checkpoint.cycles,
                    &mut checkpoint.shared_state,
                    None,
                );
                heartbeat_status.begin_commit()?;
                if let Some(result) = result {
                    checkpoint.cycle_index = cycle_index;
                    checkpoint.status = result.status;
                    checkpoint.messages = result.messages.clone();
                    checkpoint.cycles = result.cycles.clone();
                    checkpoint.shared_state = result.shared_state.clone();
                    checkpoint.terminal_result = Some(result.clone());
                    if !state_store
                        .commit_checkpoint(checkpoint, &claim_token, expected_revision)
                        .map_err(|error| error.to_string())?
                    {
                        return Err(format!(
                            "checkpoint changed while terminal cycle {cycle_index} was running for task {}",
                            task.task_id
                        ));
                    }
                    heartbeat_status.mark_commit_succeeded()?;
                    return Ok(CycleDispatchResult::finished_at_revision(
                        result,
                        Some(expected_revision + 1),
                    ));
                }

                checkpoint.cycle_index = cycle_index;
                checkpoint.status = AgentStatus::Running;
                if !state_store
                    .commit_checkpoint(checkpoint, &claim_token, expected_revision)
                    .map_err(|error| error.to_string())?
                {
                    return Err(format!(
                        "checkpoint changed while cycle {cycle_index} was running for task {}",
                        task.task_id
                    ));
                }
                heartbeat_status.mark_commit_succeeded()?;
                Ok(CycleDispatchResult::unfinished())
            })();
            let claim_committed = cycle_result.is_ok();
            LeaseOperationResult::new(cycle_result, claim_committed)
        },
    )?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::state::{Checkpoint, InMemoryStateStore, StateStoreSpec};

    struct RejectRenewStateStore {
        inner: InMemoryStateStore,
    }

    impl StateStore for RejectRenewStateStore {
        fn create_checkpoint(&self, checkpoint: Checkpoint) -> std::io::Result<bool> {
            self.inner.create_checkpoint(checkpoint)
        }

        fn save_checkpoint(&self, checkpoint: Checkpoint) -> std::io::Result<()> {
            self.inner.save_checkpoint(checkpoint)
        }

        fn load_checkpoint(&self, task_id: &str) -> std::io::Result<Option<Checkpoint>> {
            self.inner.load_checkpoint(task_id)
        }

        fn claim_checkpoint(
            &self,
            task_id: &str,
            cycle_index: u32,
            claim_token: &str,
            lease_expires_at_ms: u64,
            now_ms: u64,
        ) -> std::io::Result<Option<Checkpoint>> {
            self.inner.claim_checkpoint(
                task_id,
                cycle_index,
                claim_token,
                lease_expires_at_ms,
                now_ms,
            )
        }

        fn commit_checkpoint(
            &self,
            _checkpoint: Checkpoint,
            _claim_token: &str,
            _expected_revision: u64,
        ) -> std::io::Result<bool> {
            panic!("cycle must not commit after initial renewal failure")
        }

        fn renew_checkpoint_claim(
            &self,
            _task_id: &str,
            _claim_token: &str,
            _expected_revision: u64,
            _lease_expires_at_ms: u64,
            _now_ms: u64,
        ) -> std::io::Result<bool> {
            Ok(false)
        }

        fn finalize_checkpoint(
            &self,
            _checkpoint: Checkpoint,
            _expected_revision: u64,
        ) -> std::io::Result<bool> {
            unreachable!("checkpointed cycle does not finalize unclaimed state")
        }

        fn delete_checkpoint(&self, task_id: &str) -> std::io::Result<()> {
            self.inner.delete_checkpoint(task_id)
        }

        fn acknowledge_terminal(
            &self,
            task_id: &str,
            expected_revision: u64,
        ) -> std::io::Result<bool> {
            self.inner.acknowledge_terminal(task_id, expected_revision)
        }

        fn list_checkpoints(&self) -> std::io::Result<Vec<String>> {
            self.inner.list_checkpoints()
        }

        fn state_store_spec(&self) -> Option<StateStoreSpec> {
            None
        }
    }

    #[test]
    fn public_checkpointed_cycle_requires_initial_renewal_before_operation() {
        let store = RejectRenewStateStore {
            inner: InMemoryStateStore::new(),
        };
        let task = AgentTask::new("legacy-worker-heartbeat", "model", "system", "prompt");
        store
            .save_checkpoint(Checkpoint {
                task_id: task.task_id.clone(),
                cycle_index: 0,
                status: AgentStatus::Running,
                messages: vec![Message::user("prompt")],
                cycles: Vec::new(),
                shared_state: Metadata::new(),
                revision: 0,
                claim_token: None,
                claimed_cycle: None,
                lease_expires_at_ms: None,
                terminal_result: None,
            })
            .expect("seed checkpoint");
        let mut operation_called = false;

        let error = run_checkpointed_cycle(
            &store,
            &task,
            1,
            |_cycle, _messages, _cycles, _state, _cancellation| {
                operation_called = true;
                None
            },
        )
        .expect_err("initial renewal rejection must fail");

        assert_eq!(
            error,
            "checkpoint lease heartbeat failed: claim is no longer active"
        );
        assert!(!operation_called);
    }
}
