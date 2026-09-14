//! In-memory checkpoint store.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use crate::checkpoint::{
    notification_id_for, record_id_for, CheckpointError, CheckpointResult, ClaimMode,
    ControllerCommand, ControllerCommandReceipt, ControllerCommandResolution,
    ControllerCommandVariant, ControllerCommandWake, ControllerCommandWakeRecord, EventCursor,
    HostInteractionNotificationPayload, HostInteractionNotificationRecord, HostInteractionOutcome,
    HostInteractionRecord, HostInteractionRecoveryEnvelope, HostInteractionRecoveryResult,
    HostInteractionRequest, HostInteractionResponse, NotificationOutboxState, ResumeObservation,
    SuspendedOrigin, HOST_INTERACTION_NOTIFICATION_SCHEMA, HOST_INTERACTION_RECORD_SCHEMA,
};
use crate::events::{EventId, RunEvent, RunEventPayload};
use crate::runtime::state::{
    apply_claim, claim_candidate, close_unresolved_tools, prepare_ack, prepare_commit,
    prepare_event_delivery, prepare_finalize, prepare_finalize_claimed, prepare_progress,
    prepare_suspend, prepare_tool_receipt, Checkpoint, CheckpointStore,
};
use crate::runtime::stores::controller_helpers::{
    append_cancel_requested_event, append_control_event, append_control_event_with_result,
    control_result,
};
use crate::types::CompletionReason;

#[derive(Debug, Clone, Default)]
struct ControllerLedger {
    host_interactions: BTreeMap<String, HostInteractionRecord>,
    notifications: BTreeMap<String, HostInteractionNotificationRecord>,
    command_receipts: BTreeMap<String, (ControllerCommandReceipt, ControllerCommandResolution)>,
    commands: BTreeMap<String, ControllerCommand>,
    wake_leases: BTreeMap<String, WakeLease>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WakeLease {
    claim_token: String,
    lease_expires_at_ms: u64,
}

#[derive(Debug, Clone, Default)]
struct ArchivedHistory {
    payloads: Vec<String>,
    call_ids: BTreeSet<String>,
}

#[derive(Debug, Clone, Default)]
pub struct InMemoryCheckpointStore {
    checkpoints: Arc<Mutex<BTreeMap<String, Checkpoint>>>,
    history: Arc<Mutex<BTreeMap<String, ArchivedHistory>>>,
    deferred_receipts: Arc<Mutex<BTreeMap<String, crate::checkpoint::DeferredReceipt>>>,
    controller_ledger: Arc<Mutex<ControllerLedger>>,
}

impl InMemoryCheckpointStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn save_checkpoint(&self, checkpoint: Checkpoint) -> CheckpointResult<()> {
        checkpoint.validate()?;
        let mut checkpoints = self.lock()?;
        let checkpoint = self.persist_history(checkpoint)?;
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

    fn persist_history(&self, mut checkpoint: Checkpoint) -> CheckpointResult<Checkpoint> {
        let mut history = self.history.lock().map_err(|_| {
            CheckpointError::new(
                "checkpoint_store_lock_poisoned",
                "checkpoint history lock poisoned",
            )
        })?;
        if history
            .get(&checkpoint.checkpoint_key)
            .is_some_and(|archive| {
                checkpoint
                    .model_calls
                    .iter()
                    .any(|record| archive.call_ids.contains(&record.call_id))
            })
        {
            return Err(CheckpointError::new(
                "checkpoint_history_invalid",
                "model call identity is already archived",
            ));
        }
        if let Some(batch) = crate::runtime::state::normalize_checkpoint_history(&mut checkpoint)? {
            let archive = history
                .entry(checkpoint.checkpoint_key.clone())
                .or_default();
            if archive.payloads.len() as u64 + 1 != batch.sequence {
                return Err(CheckpointError::new(
                    "checkpoint_history_invalid",
                    "history cursor does not match archive",
                ));
            }
            let payload = String::from_utf8(crate::checkpoint::canonical_json_bytes(
                &batch.payload,
                "checkpoint history",
            )?)
            .expect("canonical JSON is UTF-8");
            for record in batch.payload["model_calls"]
                .as_array()
                .expect("typed history records")
            {
                archive.call_ids.insert(
                    record["call_id"]
                        .as_str()
                        .expect("typed model call identity")
                        .to_string(),
                );
            }
            archive.payloads.push(payload);
        }
        Ok(checkpoint)
    }

    fn receipt_lock(
        &self,
    ) -> CheckpointResult<
        std::sync::MutexGuard<'_, BTreeMap<String, crate::checkpoint::DeferredReceipt>>,
    > {
        self.deferred_receipts.lock().map_err(|_| {
            CheckpointError::new(
                "checkpoint_store_lock_poisoned",
                "deferred receipt index lock poisoned",
            )
        })
    }

    fn controller_lock(&self) -> CheckpointResult<std::sync::MutexGuard<'_, ControllerLedger>> {
        self.controller_ledger.lock().map_err(|_| {
            CheckpointError::new(
                "checkpoint_store_lock_poisoned",
                "controller ledger lock poisoned",
            )
        })
    }
}

fn resolution_with_receipt(
    resolution: &ControllerCommandResolution,
    receipt: ControllerCommandReceipt,
) -> ControllerCommandResolution {
    match resolution {
        ControllerCommandResolution::Applied { wake, .. } => ControllerCommandResolution::Applied {
            receipt,
            wake: wake.clone(),
        },
        ControllerCommandResolution::Replayed { wake, .. } => {
            ControllerCommandResolution::Replayed {
                receipt,
                wake: wake.clone(),
            }
        }
        ControllerCommandResolution::Rejected { error } => ControllerCommandResolution::Rejected {
            error: error.clone(),
        },
    }
}

include!("memory_impl.rs");
include!("memory_controller.rs");
