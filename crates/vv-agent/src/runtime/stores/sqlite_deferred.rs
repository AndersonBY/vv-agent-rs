//! SQLite's durable deferred-tool transaction helpers.

use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};

use crate::checkpoint::{
    CheckpointError, CheckpointResult, DeferredReceipt, DeferredReceiptStatus,
    DeferredResolveDecision, OperationState,
};
use crate::types::{ToolExecutionResult, ToolResultStatus};

use super::{load_row_transaction, sqlite_error, update_row, SqlValues, SqliteCheckpointStore};

pub(super) const CREATE_DEFERRED_RECEIPTS_TABLE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS deferred_resolution_receipts (
    handle_key TEXT PRIMARY KEY,
    checkpoint_key TEXT NOT NULL,
    handle TEXT NOT NULL,
    result TEXT NOT NULL,
    result_digest TEXT NOT NULL,
    event_id TEXT NOT NULL,
    event_payload_digest TEXT NOT NULL,
    receipt_status TEXT NOT NULL CHECK (receipt_status IN ('succeeded', 'failed')),
    FOREIGN KEY (checkpoint_key) REFERENCES checkpoints(checkpoint_key) ON DELETE CASCADE
)
"#;

pub(super) const CREATE_DEFERRED_RECEIPTS_INDEX_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS deferred_receipts_checkpoint_idx
    ON deferred_resolution_receipts(checkpoint_key)
"#;

pub(super) fn admit_deferred_batch(
    store: &SqliteCheckpointStore,
    checkpoint_key: &str,
    expected_revision: u64,
    claim_token: &str,
    claimed_cycle: u64,
    entries: &[crate::checkpoint::DeferredBatchEntry],
) -> CheckpointResult<crate::checkpoint::DeferredBatchAdmission> {
    let mut connection = store.lock()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let current = load_row_transaction(&transaction, checkpoint_key)?
        .ok_or_else(|| CheckpointError::new("checkpoint_not_found", "checkpoint does not exist"))?;
    let (updated, handles) = crate::runtime::state::admit_deferred_batch(
        &current,
        expected_revision,
        claim_token,
        claimed_cycle,
        entries,
    )?;
    if updated.revision == current.revision {
        transaction.commit().map_err(sqlite_error)?;
        return Err(CheckpointError::new(
            "checkpoint_revision_conflict",
            "deferred batch admission precondition failed",
        ));
    }
    let values = SqlValues::from_checkpoint(&updated)?;
    if !update_row(
        &transaction,
        &values,
        Some(expected_revision),
        Some(claim_token),
    )? {
        transaction.commit().map_err(sqlite_error)?;
        return Err(CheckpointError::new(
            "checkpoint_revision_conflict",
            "deferred batch admission compare-and-swap failed",
        ));
    }
    transaction.commit().map_err(sqlite_error)?;
    Ok(crate::checkpoint::DeferredBatchAdmission {
        checkpoint: updated,
        handles,
    })
}

pub(super) fn resolve_deferred(
    store: &SqliteCheckpointStore,
    handle: crate::checkpoint::DeferredToolHandle,
    result: ToolExecutionResult,
) -> CheckpointResult<DeferredResolveDecision> {
    crate::checkpoint::validate_definitive_result(&result)?;
    let handle_key = handle.handle_key()?;
    let mut connection = store.lock()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    if let Some(receipt) = load_receipt_transaction(&transaction, &handle_key)? {
        if crate::checkpoint::result_digest(&receipt.result)?
            == crate::checkpoint::result_digest(&result)?
        {
            transaction.commit().map_err(sqlite_error)?;
            return Ok(DeferredResolveDecision::Replayed { receipt });
        }
        transaction.commit().map_err(sqlite_error)?;
        return Err(CheckpointError::new(
            "deferred_resolution_conflict",
            "deferred handle already has a different definitive result",
        ));
    }
    let checkpoint =
        load_row_transaction(&transaction, &handle.checkpoint_key)?.ok_or_else(|| {
            CheckpointError::new("deferred_resolution_stale", "checkpoint does not exist")
        })?;
    let Some(index) = checkpoint.tool_journal.iter().position(|entry| {
        entry.operation_id == handle.operation_id
            && entry.attempt == handle.attempt
            && entry.request_digest == handle.request_digest
    }) else {
        transaction.commit().map_err(sqlite_error)?;
        return Err(CheckpointError::new(
            "deferred_resolution_stale",
            "no active journal matches the deferred handle",
        ));
    };
    let entry = &checkpoint.tool_journal[index];
    if entry.state == OperationState::Started {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(DeferredResolveDecision::not_admitted());
    }
    if entry.state == OperationState::Ambiguous {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(DeferredResolveDecision::ReconciliationRequired);
    }
    if checkpoint.claim_token.is_some() {
        transaction.commit().map_err(sqlite_error)?;
        return Err(CheckpointError::new(
            "deferred_checkpoint_claimed",
            "deferred resolution is blocked while the checkpoint is claimed",
        ));
    }
    if entry.state != OperationState::Deferred || entry.deferred_handle.as_ref() != Some(&handle) {
        transaction.commit().map_err(sqlite_error)?;
        return Err(CheckpointError::new(
            "deferred_resolution_stale",
            "deferred handle is stale or no longer active",
        ));
    }
    if entry.tool_call_id.as_deref() != Some(result.tool_call_id.as_str()) {
        transaction.commit().map_err(sqlite_error)?;
        return Err(CheckpointError::new(
            "deferred_resolution_stale",
            "deferred result tool_call_id does not match the journal",
        ));
    }
    let event = crate::runtime::state::receipt_event(&checkpoint, entry, &result)?;
    let event_id = event.event_id.clone();
    let event_digest = event.payload_digest.clone();
    let mut updated = checkpoint.clone();
    let journal = &mut updated.tool_journal[index];
    match result.status {
        ToolResultStatus::Success => {
            journal.state = OperationState::Succeeded;
            journal.result = Some(result.to_dict());
        }
        ToolResultStatus::Error => {
            journal.state = OperationState::Failed;
            journal.error = Some(crate::runtime::state::OperationError::new(
                result
                    .error_code
                    .clone()
                    .unwrap_or_else(|| "tool_error".to_string()),
                result.content.clone(),
                false,
            ));
        }
        _ => unreachable!(),
    }
    journal.deferred_handle = None;
    updated.event_outbox.push(event);
    let unresolved = updated
        .tool_journal
        .iter()
        .any(|entry| entry.state == OperationState::Deferred);
    updated.status = if unresolved {
        crate::checkpoint::CheckpointStatus::Deferred
    } else {
        crate::checkpoint::CheckpointStatus::Running
    };
    updated.revision = checkpoint
        .revision
        .checked_add(1)
        .ok_or_else(|| CheckpointError::new("checkpoint_revision_overflow", "revision overflow"))?;
    updated.validate()?;
    let receipt = DeferredReceipt::new(handle, result, event_id, event_digest)?;
    let values = SqlValues::from_checkpoint(&updated)?;
    if !update_row(&transaction, &values, Some(checkpoint.revision), None)? {
        transaction.commit().map_err(sqlite_error)?;
        return Err(CheckpointError::new(
            "checkpoint_revision_conflict",
            "deferred resolution compare-and-swap failed",
        ));
    }
    insert_receipt_transaction(&transaction, &receipt)?;
    transaction.commit().map_err(sqlite_error)?;
    Ok(if unresolved {
        DeferredResolveDecision::AppliedWaiting { receipt }
    } else {
        DeferredResolveDecision::AppliedReady { receipt }
    })
}

pub(super) fn accept_deferred_batch(
    store: &SqliteCheckpointStore,
    checkpoint_key: &str,
    expected_revision: u64,
    claim_token: &str,
    claimed_cycle: u64,
    decisions: &[crate::checkpoint::AcceptDeferredDecision],
) -> CheckpointResult<crate::checkpoint::DeferredBatchAdmission> {
    let mut connection = store.lock()?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let current = load_row_transaction(&transaction, checkpoint_key)?
        .ok_or_else(|| CheckpointError::new("checkpoint_not_found", "checkpoint does not exist"))?;
    if crate::runtime::state::deferred_batch_is_idempotent(&current, decisions) {
        transaction.commit().map_err(sqlite_error)?;
        return Ok(crate::checkpoint::DeferredBatchAdmission {
            checkpoint: current,
            handles: decisions
                .iter()
                .map(|decision| decision.handle.clone())
                .collect(),
        });
    }
    let (updated, changed) = crate::runtime::state::accept_deferred_batch(
        &current,
        expected_revision,
        claim_token,
        claimed_cycle,
        decisions,
    )?;
    if !changed {
        transaction.commit().map_err(sqlite_error)?;
        return Err(CheckpointError::new(
            "reconciliation_required",
            "deferred reconciliation precondition failed",
        ));
    }
    let values = SqlValues::from_checkpoint(&updated)?;
    if !update_row(
        &transaction,
        &values,
        Some(expected_revision),
        Some(claim_token),
    )? {
        transaction.commit().map_err(sqlite_error)?;
        return Err(CheckpointError::new(
            "checkpoint_revision_conflict",
            "deferred reconciliation compare-and-swap failed",
        ));
    }
    transaction.commit().map_err(sqlite_error)?;
    Ok(crate::checkpoint::DeferredBatchAdmission {
        checkpoint: updated,
        handles: decisions
            .iter()
            .map(|decision| decision.handle.clone())
            .collect(),
    })
}

pub(super) fn load_receipt_transaction(
    transaction: &Transaction<'_>,
    handle_key: &str,
) -> CheckpointResult<Option<DeferredReceipt>> {
    let row = transaction
        .query_row(
            "SELECT handle_key, handle, result, result_digest, event_id, event_payload_digest, receipt_status
             FROM deferred_resolution_receipts WHERE handle_key = ?1",
            params![handle_key],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .optional()
        .map_err(sqlite_error)?;
    row.map(
        |(
            handle_key,
            handle,
            result,
            result_digest,
            event_id,
            event_payload_digest,
            receipt_status,
        )| {
            let handle = serde_json::from_str(&handle).map_err(|error| {
                CheckpointError::new("deferred_receipt_invalid", error.to_string())
            })?;
            let result = serde_json::from_str(&result).map_err(|error| {
                CheckpointError::new("deferred_receipt_invalid", error.to_string())
            })?;
            let receipt_status = match receipt_status.as_str() {
                "succeeded" => DeferredReceiptStatus::Succeeded,
                "failed" => DeferredReceiptStatus::Failed,
                _ => {
                    return Err(CheckpointError::new(
                        "deferred_receipt_invalid",
                        "unknown deferred receipt status",
                    ))
                }
            };
            let receipt = DeferredReceipt {
                handle_key,
                handle,
                result,
                result_digest,
                event_id,
                event_payload_digest,
                receipt_status,
            };
            receipt.validate()?;
            Ok(receipt)
        },
    )
    .transpose()
}

pub(super) fn insert_receipt_transaction(
    transaction: &Transaction<'_>,
    receipt: &DeferredReceipt,
) -> CheckpointResult<()> {
    receipt.validate()?;
    let handle = serde_json::to_string(&receipt.handle)
        .map_err(|error| CheckpointError::new("deferred_receipt_invalid", error.to_string()))?;
    let result = serde_json::to_string(&receipt.result)
        .map_err(|error| CheckpointError::new("deferred_receipt_invalid", error.to_string()))?;
    let receipt_status = match receipt.receipt_status {
        DeferredReceiptStatus::Succeeded => "succeeded",
        DeferredReceiptStatus::Failed => "failed",
    };
    transaction
        .execute(
            "INSERT INTO deferred_resolution_receipts
             (handle_key, checkpoint_key, handle, result, result_digest, event_id, event_payload_digest, receipt_status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                receipt.handle_key,
                receipt.handle.checkpoint_key,
                handle,
                result,
                receipt.result_digest,
                receipt.event_id,
                receipt.event_payload_digest,
                receipt_status,
            ],
        )
        .map_err(sqlite_error)?;
    Ok(())
}
