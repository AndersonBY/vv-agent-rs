//! In-memory checkpoint v2 store.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::checkpoint::{CheckpointError, CheckpointResult, ClaimMode, EventCursor};
use crate::runtime::state::{
    apply_claim, claim_candidate, prepare_ack, prepare_commit, prepare_event_delivery,
    prepare_finalize, prepare_finalize_claimed, prepare_progress, prepare_suspend, Checkpoint,
    CheckpointStore,
};

#[derive(Debug, Clone, Default)]
pub struct InMemoryCheckpointStore {
    checkpoints: Arc<Mutex<BTreeMap<String, Checkpoint>>>,
}

impl InMemoryCheckpointStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn save_checkpoint(&self, checkpoint: Checkpoint) -> CheckpointResult<()> {
        checkpoint.validate()?;
        let mut checkpoints = self.lock()?;
        checkpoints.insert(checkpoint.checkpoint_key.clone(), checkpoint);
        Ok(())
    }

    fn lock(&self) -> CheckpointResult<std::sync::MutexGuard<'_, BTreeMap<String, Checkpoint>>> {
        self.checkpoints.lock().map_err(|_| {
            CheckpointError::new(
                "checkpoint_store_lock_poisoned",
                "checkpoint store lock poisoned",
            )
        })
    }
}

impl CheckpointStore for InMemoryCheckpointStore {
    fn create_checkpoint(&self, checkpoint: Checkpoint) -> CheckpointResult<bool> {
        checkpoint.validate()?;
        let mut checkpoints = self.lock()?;
        if checkpoints.contains_key(&checkpoint.checkpoint_key) {
            return Ok(false);
        }
        checkpoints.insert(checkpoint.checkpoint_key.clone(), checkpoint);
        Ok(true)
    }

    fn load_checkpoint(&self, checkpoint_key: &str) -> CheckpointResult<Option<Checkpoint>> {
        let checkpoints = self.lock()?;
        Ok(checkpoints.get(checkpoint_key).cloned())
    }

    fn claim_checkpoint(
        &self,
        checkpoint_key: &str,
        cycle_index: u64,
        claim_token: &str,
        lease_expires_at_ms: u64,
        now_ms: u64,
        claim_mode: ClaimMode,
    ) -> CheckpointResult<Option<Checkpoint>> {
        if claim_token.trim().is_empty() || lease_expires_at_ms <= now_ms {
            return Err(CheckpointError::new(
                "checkpoint_claim_invalid",
                "claim token must be non-empty and lease must be in the future",
            ));
        }
        let mut checkpoints = self.lock()?;
        let Some(current) = checkpoints.get(checkpoint_key).cloned() else {
            return Ok(None);
        };
        if !claim_candidate(&current, cycle_index, now_ms, claim_mode)? {
            return Ok(None);
        }
        let mut claimed = current;
        apply_claim(
            &mut claimed,
            cycle_index,
            claim_token,
            lease_expires_at_ms,
            claim_mode,
        )?;
        claimed.validate()?;
        checkpoints.insert(checkpoint_key.to_string(), claimed.clone());
        Ok(Some(claimed))
    }

    fn progress_checkpoint(
        &self,
        checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> CheckpointResult<bool> {
        let mut checkpoints = self.lock()?;
        let Some(current) = checkpoints.get(&checkpoint.checkpoint_key).cloned() else {
            return Ok(false);
        };
        let Some(updated) = prepare_progress(&current, checkpoint, claim_token, expected_revision)?
        else {
            return Ok(false);
        };
        checkpoints.insert(updated.checkpoint_key.clone(), updated);
        Ok(true)
    }

    fn suspend_checkpoint(
        &self,
        checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> CheckpointResult<bool> {
        let mut checkpoints = self.lock()?;
        let Some(current) = checkpoints.get(&checkpoint.checkpoint_key).cloned() else {
            return Ok(false);
        };
        let Some(updated) = prepare_suspend(&current, checkpoint, claim_token, expected_revision)?
        else {
            return Ok(false);
        };
        checkpoints.insert(updated.checkpoint_key.clone(), updated);
        Ok(true)
    }

    fn commit_checkpoint(
        &self,
        checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> CheckpointResult<bool> {
        let mut checkpoints = self.lock()?;
        let Some(current) = checkpoints.get(&checkpoint.checkpoint_key).cloned() else {
            return Ok(false);
        };
        let Some(updated) = prepare_commit(&current, checkpoint, claim_token, expected_revision)?
        else {
            return Ok(false);
        };
        checkpoints.insert(updated.checkpoint_key.clone(), updated);
        Ok(true)
    }

    fn finalize_checkpoint(
        &self,
        checkpoint: Checkpoint,
        expected_revision: u64,
    ) -> CheckpointResult<bool> {
        let mut checkpoints = self.lock()?;
        let Some(current) = checkpoints.get(&checkpoint.checkpoint_key).cloned() else {
            return Ok(false);
        };
        let Some(updated) = prepare_finalize(&current, checkpoint, expected_revision)? else {
            return Ok(false);
        };
        checkpoints.insert(updated.checkpoint_key.clone(), updated);
        Ok(true)
    }

    fn finalize_claimed_checkpoint(
        &self,
        checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> CheckpointResult<bool> {
        let mut checkpoints = self.lock()?;
        let Some(current) = checkpoints.get(&checkpoint.checkpoint_key).cloned() else {
            return Ok(false);
        };
        let Some(updated) =
            prepare_finalize_claimed(&current, checkpoint, claim_token, expected_revision)?
        else {
            return Ok(false);
        };
        checkpoints.insert(updated.checkpoint_key.clone(), updated);
        Ok(true)
    }

    fn renew_checkpoint_claim(
        &self,
        checkpoint_key: &str,
        claim_token: &str,
        lease_expires_at_ms: u64,
        now_ms: u64,
    ) -> CheckpointResult<bool> {
        if claim_token.trim().is_empty() || lease_expires_at_ms <= now_ms {
            return Err(CheckpointError::new(
                "checkpoint_claim_invalid",
                "claim token must be non-empty and lease must be in the future",
            ));
        }
        let mut checkpoints = self.lock()?;
        let Some(checkpoint) = checkpoints.get_mut(checkpoint_key) else {
            return Ok(false);
        };
        if checkpoint.claim_token.as_deref() != Some(claim_token)
            || checkpoint
                .lease_expires_at_ms
                .is_none_or(|expiry| expiry <= now_ms)
        {
            return Ok(false);
        }
        checkpoint.lease_expires_at_ms = Some(lease_expires_at_ms);
        Ok(true)
    }

    fn acknowledge_terminal(
        &self,
        checkpoint_key: &str,
        expected_revision: u64,
    ) -> CheckpointResult<bool> {
        let mut checkpoints = self.lock()?;
        let Some(current) = checkpoints.get(checkpoint_key).cloned() else {
            return Ok(false);
        };
        let Some(updated) = prepare_ack(&current, expected_revision)? else {
            return Ok(false);
        };
        checkpoints.insert(checkpoint_key.to_string(), updated);
        Ok(true)
    }

    fn record_event_delivery(
        &self,
        checkpoint_key: &str,
        claim_token: Option<&str>,
        expected_revision: u64,
        event_id: &str,
        payload_digest: &str,
        cursor: EventCursor,
    ) -> CheckpointResult<bool> {
        let mut checkpoints = self.lock()?;
        let Some(current) = checkpoints.get(checkpoint_key).cloned() else {
            return Ok(false);
        };
        let Some(updated) = prepare_event_delivery(
            &current,
            claim_token,
            expected_revision,
            event_id,
            payload_digest,
            cursor,
        )?
        else {
            return Ok(false);
        };
        checkpoints.insert(checkpoint_key.to_string(), updated);
        Ok(true)
    }

    fn delete_checkpoint(&self, checkpoint_key: &str) -> CheckpointResult<()> {
        self.lock()?.remove(checkpoint_key);
        Ok(())
    }

    fn list_checkpoints(&self) -> CheckpointResult<Vec<String>> {
        Ok(self.lock()?.keys().cloned().collect())
    }
}
