//! In-memory checkpoint store.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use crate::checkpoint::{
    notification_id_for, record_id_for, CheckpointError, CheckpointResult, ClaimMode,
    ControllerCommand, ControllerCommandReceipt, ControllerCommandResolution,
    ControllerCommandVariant, ControllerCommandWake, EventCursor,
    HostInteractionNotificationPayload, HostInteractionNotificationRecord, HostInteractionOutcome,
    HostInteractionRecord, HostInteractionRecoveryEnvelope, HostInteractionRecoveryResult,
    HostInteractionRequest, HostInteractionResponse, NotificationOutboxState, ResumeObservation,
    SuspendedOrigin, HOST_INTERACTION_NOTIFICATION_SCHEMA, HOST_INTERACTION_RECORD_SCHEMA,
};
use crate::events::{EventId, RunEvent, RunEventPayload};
use crate::runtime::state::{
    apply_claim, claim_candidate, prepare_ack, prepare_commit, prepare_event_delivery,
    prepare_finalize, prepare_finalize_claimed, prepare_progress, prepare_suspend, Checkpoint,
    CheckpointStore,
};
use crate::types::{AgentResult, CompletionReason};

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
pub struct InMemoryCheckpointStore {
    checkpoints: Arc<Mutex<BTreeMap<String, Checkpoint>>>,
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
