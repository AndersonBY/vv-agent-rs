use std::sync::atomic::{AtomicBool, AtomicUsize};

use super::*;
use crate::checkpoint::ReconciliationError;
use crate::event_store::{EventStoreError, RunEventIter, RunEventReplayQuery};
use crate::{InMemoryCheckpointStore, SqliteCheckpointStore};

const KEY: &str = "reconciliation-atomic";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    Before,
    After,
    ClaimLost,
}

struct ProbeStore {
    inner: Arc<dyn CheckpointStore>,
    fault: Mutex<Option<Fault>>,
    commits: Mutex<Vec<(Checkpoint, Checkpoint)>>,
}

impl ProbeStore {
    fn commit(
        &self,
        checkpoint: &Checkpoint,
        write: impl FnOnce() -> CheckpointResult<bool>,
    ) -> CheckpointResult<bool> {
        let resolving = audits(checkpoint).iter().any(|row| row.state == "pending");
        let fault = if resolving {
            self.fault.lock().unwrap().take()
        } else {
            None
        };
        let before = self.inner.load_checkpoint(KEY)?.unwrap();
        if fault == Some(Fault::Before) {
            return Err(CheckpointError::new("test_crash", "before resolution CAS"));
        }
        if fault == Some(Fault::ClaimLost) {
            let now = before.lease_expires_at_ms.unwrap() + 1;
            self.inner.claim_checkpoint(
                KEY,
                1,
                "replacement-owner",
                now + 10_000,
                now,
                ClaimMode::Recovery,
            )?;
        }
        let applied = write()?;
        if applied && resolving {
            let after = self.inner.load_checkpoint(KEY)?.unwrap();
            self.commits.lock().unwrap().push((before, after));
            if fault == Some(Fault::After) {
                return Err(CheckpointError::new("test_crash", "after resolution CAS"));
            }
        }
        Ok(applied)
    }
}

impl CheckpointStore for ProbeStore {
    fn create_checkpoint(&self, checkpoint: Checkpoint) -> CheckpointResult<bool> {
        self.inner.create_checkpoint(checkpoint)
    }
    fn load_checkpoint(&self, key: &str) -> CheckpointResult<Option<Checkpoint>> {
        self.inner.load_checkpoint(key)
    }
    fn claim_checkpoint(
        &self,
        key: &str,
        cycle: u64,
        token: &str,
        expiry: u64,
        now: u64,
        mode: ClaimMode,
    ) -> CheckpointResult<Option<Checkpoint>> {
        self.inner
            .claim_checkpoint(key, cycle, token, expiry, now, mode)
    }
    fn progress_checkpoint(
        &self,
        checkpoint: Checkpoint,
        token: &str,
        revision: u64,
    ) -> CheckpointResult<bool> {
        self.commit(&checkpoint, || {
            self.inner
                .progress_checkpoint(checkpoint.clone(), token, revision)
        })
    }
    fn suspend_checkpoint(
        &self,
        checkpoint: Checkpoint,
        token: &str,
        revision: u64,
    ) -> CheckpointResult<bool> {
        self.inner.suspend_checkpoint(checkpoint, token, revision)
    }
    fn commit_checkpoint(
        &self,
        checkpoint: Checkpoint,
        token: &str,
        revision: u64,
    ) -> CheckpointResult<bool> {
        self.inner.commit_checkpoint(checkpoint, token, revision)
    }
    fn finalize_claimed_checkpoint(
        &self,
        checkpoint: Checkpoint,
        token: &str,
        revision: u64,
    ) -> CheckpointResult<bool> {
        self.inner
            .finalize_claimed_checkpoint(checkpoint, token, revision)
    }
    fn finalize_checkpoint(&self, checkpoint: Checkpoint, revision: u64) -> CheckpointResult<bool> {
        self.inner.finalize_checkpoint(checkpoint, revision)
    }
    fn renew_checkpoint_claim(
        &self,
        key: &str,
        token: &str,
        expiry: u64,
        now: u64,
    ) -> CheckpointResult<CheckpointRenewalOutcome> {
        self.inner.renew_checkpoint_claim(key, token, expiry, now)
    }
    fn record_tool_receipt(
        &self,
        checkpoint: Checkpoint,
        operation: &str,
        attempt: u64,
        call: &str,
        digest: &str,
        result: ToolExecutionResult,
        token: &str,
        revision: u64,
        cycle: u64,
    ) -> CheckpointResult<bool> {
        self.commit(&checkpoint, || {
            self.inner.record_tool_receipt(
                checkpoint.clone(),
                operation,
                attempt,
                call,
                digest,
                result,
                token,
                revision,
                cycle,
            )
        })
    }
    fn record_event_delivery(
        &self,
        key: &str,
        token: Option<&str>,
        revision: u64,
        id: &str,
        digest: &str,
        cursor: EventCursor,
    ) -> CheckpointResult<bool> {
        self.inner
            .record_event_delivery(key, token, revision, id, digest, cursor)
    }
    fn acknowledge_terminal(&self, key: &str, revision: u64) -> CheckpointResult<bool> {
        self.inner.acknowledge_terminal(key, revision)
    }
    fn delete_checkpoint(&self, key: &str) -> CheckpointResult<()> {
        self.inner.delete_checkpoint(key)
    }
    fn list_checkpoints(&self) -> CheckpointResult<Vec<String>> {
        self.inner.list_checkpoints()
    }
}

struct Provider {
    decision: ReconciliationDecision,
    calls: AtomicUsize,
}

impl ReconciliationProvider for Provider {
    fn reconcile(&self, _: &ResumeObservation) -> CheckpointResult<ReconciliationDecision> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.decision.clone())
    }
}

#[derive(Default)]
struct Events {
    rows: Mutex<BTreeMap<String, (String, Value, EventCursor)>>,
    fail_after_append: AtomicBool,
}

impl RunEventStore for Events {
    fn append(&self, _: &RunEvent) -> Result<(), EventStoreError> {
        panic!("checkpoint delivery must use append_once")
    }
    fn replay(&self, _: RunEventReplayQuery) -> Result<RunEventIter, EventStoreError> {
        Ok(Box::new(std::iter::empty()))
    }
    fn append_once(
        &self,
        id: &str,
        digest: &str,
        event: &RunEvent,
    ) -> Result<EventCursor, EventStoreError> {
        let payload = serde_json::to_value(event).unwrap();
        let mut rows = self.rows.lock().unwrap();
        if let Some((saved_digest, saved_event, cursor)) = rows.get(id) {
            assert_eq!(saved_digest, digest);
            assert_eq!(saved_event, &payload);
            return Ok(cursor.clone());
        }
        let cursor = EventCursor::new(
            CapabilityRef::new("test.reconciliation-events", "1").unwrap(),
            json!({"sequence": rows.len() + 1}),
            Some(id.to_string()),
        );
        rows.insert(
            id.to_string(),
            (digest.to_string(), payload.clone(), cursor.clone()),
        );
        if payload["type"] == "reconciliation_resolved"
            && self.fail_after_append.swap(false, Ordering::SeqCst)
        {
            return Err(EventStoreError::new(
                "test_crash",
                "after append before delivery acknowledgement",
            ));
        }
        Ok(cursor)
    }
}

fn seed(sqlite: bool) -> (Arc<ProbeStore>, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let inner: Arc<dyn CheckpointStore> = if sqlite {
        Arc::new(SqliteCheckpointStore::new(directory.path().join("recovery.sqlite3")).unwrap())
    } else {
        Arc::new(InMemoryCheckpointStore::new())
    };
    let store = Arc::new(ProbeStore {
        inner,
        fault: Mutex::new(None),
        commits: Mutex::new(vec![]),
    });
    let codec: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/parity/checkpoint_codec.json"
    )))
    .unwrap();
    let mut payload = codec["valid_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "minimal_running")
        .unwrap()["payload"]
        .clone();
    payload["checkpoint_key"] = json!(KEY);
    let checkpoint =
        crate::runtime::checkpoint_codec::checkpoint_from_value(&payload, 262_144).unwrap();
    assert!(store.create_checkpoint(checkpoint).unwrap());
    let mut claimed = store
        .claim_checkpoint(KEY, 1, "seed-owner", 200, 100, ClaimMode::Continue)
        .unwrap()
        .unwrap();
    let journal: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/parity/operation_journal.json"
    )))
    .unwrap();
    let entry = &journal["valid_entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "tool_started")
        .unwrap()["entry"];
    let mut entry = OperationJournalEntry::from_value(entry).unwrap();
    entry.cycle_index = 1;
    entry.state = OperationState::Ambiguous;
    claimed.tool_journal = vec![entry];
    assert!(store
        .progress_checkpoint(claimed.clone(), "seed-owner", claimed.revision)
        .unwrap());
    (store, directory)
}

fn controller(
    store: Arc<ProbeStore>,
    provider: Arc<Provider>,
    events: Option<Arc<Events>>,
) -> CheckpointResumeController {
    let before = store.load_checkpoint(KEY).unwrap().unwrap();
    let now = before
        .lease_expires_at_ms
        .unwrap_or_default()
        .max(now_ms().unwrap())
        + 1;
    let token = format!("recovery-{}", before.resume_attempt);
    let claimed = store
        .claim_checkpoint(KEY, 1, &token, now + 10_000, now, ClaimMode::Recovery)
        .unwrap()
        .unwrap();
    let mut controller = CheckpointResumeController::new(CheckpointControllerRequest {
        config: CheckpointConfig {
            store: Some(store),
            key: Some(KEY.to_string()),
            resume_policy: ResumePolicy::RequireExisting,
            ..CheckpointConfig::default()
        },
        task_id: claimed.task_id.clone(),
        run_id: claimed.root_run_id.clone(),
        trace_id: claimed.trace_id.clone(),
        agent_name: "reconciliation-test".to_string(),
        run_definition: claimed.run_definition.clone(),
        run_definition_digest: claimed.run_definition_digest.clone(),
        initial_messages: vec![],
        initial_shared_state: BTreeMap::new(),
        initial_budget_usage: None,
        extensions: vec![],
        reconciliation_provider: Some(provider),
        event_sink: Arc::new(|_| Ok(())),
        event_store: events.map(|events| events as Arc<dyn RunEventStore>),
        preloaded_checkpoint: None,
    })
    .unwrap();
    // The real store owns the preclaimed worker fence; no heartbeat sleeps or
    // external dispatch are needed to execute the actual recovery producer.
    controller.checkpoint = Some(claimed);
    controller.owned_claim_token = Some(token);
    controller.deliver_pending_outbox().unwrap();
    controller
}

fn provider(kind: &str, entry: &OperationJournalEntry) -> Arc<Provider> {
    let decision = match kind {
        "retry" => ReconciliationDecision::retry(),
        "record_failure" => ReconciliationDecision::record_failure(ReconciliationError::new(
            "tool_unavailable",
            "still unavailable",
            true,
        )),
        "replay_success" => ReconciliationDecision {
            kind: ReconciliationDecisionKind::ReplaySuccess,
            response: None,
            result: Some(
                ToolExecutionResult::success(
                    entry.tool_call_id.as_deref().unwrap(),
                    "retained result",
                )
                .to_dict(),
            ),
            error: None,
            handle: None,
        },
        _ => unreachable!(),
    };
    Arc::new(Provider {
        decision,
        calls: AtomicUsize::new(0),
    })
}

fn audits(checkpoint: &Checkpoint) -> Vec<&EventOutboxEntry> {
    checkpoint
        .event_outbox
        .iter()
        .filter(|row| row.event["type"] == "reconciliation_resolved")
        .collect()
}

#[test]
fn retry_then_ambiguous_resolution_commits_receipt_and_audit_once() {
    for sqlite in [false, true] {
        for resolution in ["record_failure", "replay_success"] {
            let (store, _directory) = seed(sqlite);
            let before = store.load_checkpoint(KEY).unwrap().unwrap();
            let mut first = controller(
                store.clone(),
                provider("retry", &before.tool_journal[0]),
                None,
            );
            assert!(first.recover_ambiguous_operations().unwrap().is_none());
            let checkpoint = first.require_checkpoint().unwrap();
            let entry = &checkpoint.tool_journal[0];
            assert_eq!((entry.attempt, entry.state), (2, OperationState::Planned));
            assert_eq!(
                audits(checkpoint)[0].event_id,
                stable_event_id_for(
                    KEY,
                    "reconciliation_resolved",
                    &[&entry.operation_id, "1", "retry"]
                )
            );
            first.require_checkpoint_mut().unwrap().tool_journal[0].state =
                OperationState::Ambiguous;
            first.progress().unwrap();
            let entry = first.require_checkpoint().unwrap().tool_journal[0].clone();
            drop(first);
            let mut second = controller(store.clone(), provider(resolution, &entry), None);
            assert!(second.recover_ambiguous_operations().unwrap().is_none());
            let checkpoint = second.require_checkpoint().unwrap();
            let rows = audits(checkpoint);
            assert_eq!(rows.len(), 2);
            assert_ne!(rows[0].event_id, rows[1].event_id);
            assert_eq!(
                rows[1].event_id,
                stable_event_id_for(
                    KEY,
                    "reconciliation_resolved",
                    &[&entry.operation_id, "2", resolution]
                )
            );
            let entry = &checkpoint.tool_journal[0];
            assert_eq!(entry.attempt, 2);
            assert!(entry.result.is_some() && entry.result_digest.is_some());
            if resolution == "record_failure" {
                assert_eq!(entry.state, OperationState::Failed);
                assert_eq!(
                    entry.result.as_ref().unwrap()["metadata"],
                    json!({"retryable": true})
                );
                assert!(entry.error.as_ref().unwrap().retryable);
            } else {
                assert_eq!(entry.state, OperationState::Succeeded);
            }
            let commits = store.commits.lock().unwrap();
            assert_eq!(commits.len(), 2);
            for (before, after) in commits.iter() {
                assert_eq!(after.revision, before.revision + 1);
                assert_eq!(after.claim_token, before.claim_token);
                assert_eq!(audits(after).len(), audits(before).len() + 1);
                assert_eq!(audits(after).last().unwrap().state, "pending");
            }
        }
    }
}

#[test]
fn retained_destination_attempt_retry_preserves_original_event_bytes() {
    for sqlite in [false, true] {
        for delivered in [false, true] {
            for resolution in ["record_failure", "replay_success"] {
                let (store, _directory) = seed(sqlite);
                let before = store.load_checkpoint(KEY).unwrap().unwrap();
                let mut first = controller(
                    store.clone(),
                    provider("retry", &before.tool_journal[0]),
                    None,
                );
                first.require_checkpoint_mut().unwrap().tool_journal[0].attempt = 2;
                let entry = first.require_checkpoint().unwrap().tool_journal[0].clone();
                let mut old = first
                    .checkpoint_event(
                        1,
                        RunEventPayload::ReconciliationResolved {
                            checkpoint_key: KEY.to_string(),
                            operation_id: entry.operation_id.clone(),
                            operation_kind: entry.kind,
                            decision: ReconciliationDecisionKind::Retry,
                            claim_mode: None,
                        },
                        stable_event_id_for(
                            KEY,
                            "reconciliation_resolved",
                            &[&entry.operation_id, "2"],
                        ),
                    )
                    .unwrap();
                old.created_at = 123.0;
                first.queue_outbox_event(old).unwrap();
                first.progress().unwrap();
                if delivered {
                    first.deliver_pending_outbox().unwrap();
                }
                let original = audits(first.require_checkpoint().unwrap())[0].clone();
                drop(first);
                let mut second = controller(store.clone(), provider(resolution, &entry), None);
                second.recover_ambiguous_operations().unwrap();
                let rows = audits(second.require_checkpoint().unwrap());
                assert_eq!(rows.len(), 2);
                assert_eq!(
                    (&rows[0].event_id, &rows[0].event, &rows[0].payload_digest),
                    (
                        &original.event_id,
                        &original.event,
                        &original.payload_digest
                    )
                );
                assert_ne!(rows[0].event_id, rows[1].event_id);
                assert_eq!(rows[1].event["decision"], resolution);
            }
        }
    }
}

#[test]
fn resolution_crash_recovery_reuses_committed_audit_without_reconsulting_provider() {
    for sqlite in [false, true] {
        for resolution in ["retry", "record_failure", "replay_success"] {
            for fault in [Some(Fault::Before), Some(Fault::After), None] {
                let (store, _directory) = seed(sqlite);
                let before = store.load_checkpoint(KEY).unwrap().unwrap();
                let provider = provider(resolution, &before.tool_journal[0]);
                let events = Arc::new(Events::default());
                events
                    .fail_after_append
                    .store(fault.is_none(), Ordering::SeqCst);
                *store.fault.lock().unwrap() = fault;
                let mut first = controller(store.clone(), provider.clone(), Some(events.clone()));
                assert!(first.recover_ambiguous_operations().is_err());
                drop(first);
                let crashed = store.load_checkpoint(KEY).unwrap().unwrap();
                let saved: Vec<_> = audits(&crashed).into_iter().cloned().collect();
                if fault == Some(Fault::Before) {
                    assert!(saved.is_empty());
                    assert_eq!(
                        (
                            crashed.tool_journal[0].state,
                            crashed.tool_journal[0].attempt
                        ),
                        (OperationState::Ambiguous, 1)
                    );
                    assert!(crashed.tool_journal[0].result.is_none());
                } else {
                    assert_eq!(saved.len(), 1);
                    assert_eq!(saved[0].state, "pending");
                    assert_ne!(crashed.tool_journal[0].state, OperationState::Ambiguous);
                }
                let mut second = controller(store.clone(), provider.clone(), Some(events.clone()));
                assert!(second.recover_ambiguous_operations().unwrap().is_none());
                let rows = audits(second.require_checkpoint().unwrap());
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].state, "delivered");
                if !saved.is_empty() {
                    assert_eq!(rows[0].event, saved[0].event);
                    assert_eq!(rows[0].payload_digest, saved[0].payload_digest);
                }
                assert_eq!(
                    provider.calls.load(Ordering::SeqCst),
                    if fault == Some(Fault::Before) { 2 } else { 1 }
                );
                assert_eq!(
                    events
                        .rows
                        .lock()
                        .unwrap()
                        .values()
                        .filter(|row| row.1["type"] == "reconciliation_resolved")
                        .count(),
                    1
                );
                assert_eq!(store.commits.lock().unwrap().len(), 1);
            }
        }
    }
}

#[test]
fn lost_claim_cannot_commit_resolution_or_audit() {
    for sqlite in [false, true] {
        for resolution in ["retry", "record_failure", "replay_success"] {
            let (store, _directory) = seed(sqlite);
            let before = store.load_checkpoint(KEY).unwrap().unwrap();
            let mut controller = controller(
                store.clone(),
                provider(resolution, &before.tool_journal[0]),
                None,
            );
            *store.fault.lock().unwrap() = Some(Fault::ClaimLost);
            let error = controller.recover_ambiguous_operations().unwrap_err();
            assert!(matches!(
                error.code(),
                "checkpoint_store_conflict" | "checkpoint_claim_conflict"
            ));
            let retained = store.load_checkpoint(KEY).unwrap().unwrap();
            assert_eq!(retained.claim_token.as_deref(), Some("replacement-owner"));
            assert_eq!(
                (
                    retained.tool_journal[0].state,
                    retained.tool_journal[0].attempt
                ),
                (OperationState::Ambiguous, 1)
            );
            assert!(retained.tool_journal[0].result.is_none());
            assert!(audits(&retained).is_empty());
            assert!(store.commits.lock().unwrap().is_empty());
        }
    }
}

#[test]
fn reconciled_receipt_replay_still_rejects_different_result() {
    for sqlite in [false, true] {
        let (store, _directory) = seed(sqlite);
        let before = store.load_checkpoint(KEY).unwrap().unwrap();
        let mut controller = controller(
            store.clone(),
            provider("replay_success", &before.tool_journal[0]),
            None,
        );
        let stale = controller.require_checkpoint().unwrap().clone();
        controller.recover_ambiguous_operations().unwrap();
        let retained = store.load_checkpoint(KEY).unwrap().unwrap();
        let entry = &retained.tool_journal[0];
        let result = ToolExecutionResult::from_dict(entry.result.as_ref().unwrap()).unwrap();
        let write = |result| {
            store.record_tool_receipt(
                stale.clone(),
                &entry.operation_id,
                entry.attempt,
                entry.tool_call_id.as_deref().unwrap(),
                &entry.request_digest,
                result,
                stale.claim_token.as_deref().unwrap(),
                stale.revision,
                1,
            )
        };
        assert!(write(result.clone()).unwrap());
        assert_eq!(store.load_checkpoint(KEY).unwrap().unwrap(), retained);
        let mut different = result;
        different.content = "different result".to_string();
        assert_eq!(
            write(different).unwrap_err().code(),
            "tool_receipt_conflict"
        );
        assert_eq!(store.load_checkpoint(KEY).unwrap().unwrap(), retained);
    }
}

#[test]
fn resolved_event_timestamp_replay_preserves_bytes_and_rejects_payload_change() {
    let (store, _directory) = seed(false);
    let before = store.load_checkpoint(KEY).unwrap().unwrap();
    let mut controller = controller(
        store.clone(),
        provider("retry", &before.tool_journal[0]),
        None,
    );
    controller.recover_ambiguous_operations().unwrap();
    let original = audits(controller.require_checkpoint().unwrap())[0].clone();
    let mut replay = original.event.clone();
    replay["created_at"] = json!(original.event["created_at"].as_f64().unwrap() + 100.0);
    controller
        .queue_outbox_event(serde_json::from_value(replay).unwrap())
        .unwrap();
    assert_eq!(
        audits(controller.require_checkpoint().unwrap())[0],
        &original
    );
    let mut different = original.event.clone();
    different["metadata"] = json!({"changed": true});
    let error = controller
        .queue_outbox_event(serde_json::from_value(different).unwrap())
        .unwrap_err();
    assert_eq!(error.code(), "event_identity_conflict");
    assert_eq!(
        audits(controller.require_checkpoint().unwrap())[0],
        &original
    );
}

#[test]
fn nondefinitive_reconciliation_cannot_commit_a_resolved_audit() {
    let (store, _directory) = seed(false);
    let before = store.load_checkpoint(KEY).unwrap().unwrap();
    let mut value = provider("replay_success", &before.tool_journal[0])
        .decision
        .clone();
    value.result = Some(
        ToolExecutionResult::error(
            before.tool_journal[0].tool_call_id.as_deref().unwrap(),
            "unknown",
        )
        .with_error_code("tool_execution_failed")
        .to_dict(),
    );
    let provider = Arc::new(Provider {
        decision: value,
        calls: AtomicUsize::new(0),
    });
    let mut controller = controller(store.clone(), provider, None);
    assert!(controller.recover_ambiguous_operations().is_err());
    let retained = store.load_checkpoint(KEY).unwrap().unwrap();
    assert_eq!(retained.tool_journal[0].state, OperationState::Ambiguous);
    assert!(audits(&retained).is_empty());
}
