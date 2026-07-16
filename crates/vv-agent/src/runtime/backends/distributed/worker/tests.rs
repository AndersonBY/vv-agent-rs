use super::*;
use crate::runtime::backends::RuntimeRecipe;
use crate::runtime::state::InMemoryStateStore;
use crate::types::{AgentTask, Message};
use serde_json::Value;
use std::time::Instant;

const DISTRIBUTED_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/parity/distributed_run_envelope_v1.json"
));

fn lease_lifecycle() -> Value {
    serde_json::from_str::<Value>(DISTRIBUTED_FIXTURE).expect("distributed fixture")
        ["lease_lifecycle"]
        .clone()
}

fn worker_case(name: &str) -> Value {
    lease_lifecycle()["worker_cases"]
        .as_array()
        .expect("worker cases")
        .iter()
        .find(|case| case["name"] == name)
        .unwrap_or_else(|| panic!("missing worker case {name}"))
        .clone()
}

fn claimed_checkpoint(
    task: &AgentTask,
    initial_lease_ms: u64,
) -> (Arc<InMemoryStateStore>, Checkpoint, u64) {
    let store = Arc::new(InMemoryStateStore::new());
    store
        .create_checkpoint(Checkpoint {
            task_id: task.task_id.clone(),
            cycle_index: 0,
            status: AgentStatus::Running,
            messages: vec![Message::system("system"), Message::user("prompt")],
            cycles: Vec::new(),
            shared_state: Metadata::new(),
            revision: 0,
            claim_token: None,
            claimed_cycle: None,
            lease_expires_at_ms: None,
            terminal_result: None,
            budget_usage: None,
        })
        .expect("create checkpoint");
    let now_ms = now_unix_ms().expect("current time");
    let initial_expiry = now_ms
        .checked_add(initial_lease_ms)
        .expect("initial lease expiry");
    let claimed = store
        .claim_checkpoint(&task.task_id, 1, "owner", initial_expiry, now_ms)
        .expect("claim checkpoint")
        .expect("claimed checkpoint");
    (store, claimed, initial_expiry)
}

fn envelope(task: AgentTask, lease_duration_ms: u64) -> DistributedRunEnvelope {
    DistributedRunEnvelope::for_cycle(
        task,
        RuntimeRecipe::new("unused.json", "test", "model", "."),
        1,
        super::super::DEFAULT_CYCLE_NAME,
        None,
        None,
        lease_duration_ms,
        None,
    )
    .expect("distributed envelope")
}

struct BlockingRenewStateStore {
    inner: Arc<InMemoryStateStore>,
    renewal_calls: std::sync::atomic::AtomicUsize,
    periodic_started: std::sync::mpsc::Sender<()>,
    release_periodic: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl BlockingRenewStateStore {
    fn new(
        inner: Arc<InMemoryStateStore>,
    ) -> (
        Arc<Self>,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (periodic_started, periodic_started_rx) = std::sync::mpsc::channel();
        let (release_periodic, release_periodic_rx) = std::sync::mpsc::channel();
        (
            Arc::new(Self {
                inner,
                renewal_calls: std::sync::atomic::AtomicUsize::new(0),
                periodic_started,
                release_periodic: Mutex::new(release_periodic_rx),
            }),
            periodic_started_rx,
            release_periodic,
        )
    }
}

impl StateStore for BlockingRenewStateStore {
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
        checkpoint: Checkpoint,
        claim_token: &str,
        expected_revision: u64,
    ) -> std::io::Result<bool> {
        self.inner
            .commit_checkpoint(checkpoint, claim_token, expected_revision)
    }

    fn renew_checkpoint_claim(
        &self,
        task_id: &str,
        claim_token: &str,
        expected_revision: u64,
        lease_expires_at_ms: u64,
        now_ms: u64,
    ) -> std::io::Result<bool> {
        let call_index = self
            .renewal_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call_index == 0 {
            return self.inner.renew_checkpoint_claim(
                task_id,
                claim_token,
                expected_revision,
                lease_expires_at_ms,
                now_ms,
            );
        }
        self.periodic_started
            .send(())
            .expect("signal blocked periodic renewal");
        self.release_periodic
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recv_timeout(Duration::from_secs(30))
            .expect("release blocked periodic renewal");
        Ok(false)
    }

    fn finalize_checkpoint(
        &self,
        checkpoint: Checkpoint,
        expected_revision: u64,
    ) -> std::io::Result<bool> {
        self.inner
            .finalize_checkpoint(checkpoint, expected_revision)
    }

    fn delete_checkpoint(&self, task_id: &str) -> std::io::Result<()> {
        self.inner.delete_checkpoint(task_id)
    }

    fn acknowledge_terminal(&self, task_id: &str, expected_revision: u64) -> std::io::Result<bool> {
        self.inner.acknowledge_terminal(task_id, expected_revision)
    }

    fn list_checkpoints(&self) -> std::io::Result<Vec<String>> {
        self.inner.list_checkpoints()
    }

    fn state_store_spec(&self) -> Option<crate::runtime::state::StateStoreSpec> {
        self.inner.state_store_spec()
    }
}

struct StartupClaimViewStore {
    checkpoint: Option<Checkpoint>,
    renew_result: bool,
    renew_error: Option<&'static str>,
    renew_delay: Duration,
    renewal_calls: std::sync::atomic::AtomicUsize,
}

impl StartupClaimViewStore {
    fn new(checkpoint: Option<Checkpoint>) -> Self {
        Self::with_renew_result(checkpoint, true)
    }

    fn with_renew_result(checkpoint: Option<Checkpoint>, renew_result: bool) -> Self {
        Self {
            checkpoint,
            renew_result,
            renew_error: None,
            renew_delay: Duration::ZERO,
            renewal_calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn with_delayed_renewal(
        checkpoint: Option<Checkpoint>,
        renew_delay: Duration,
        renew_result: bool,
    ) -> Self {
        Self {
            checkpoint,
            renew_result,
            renew_error: None,
            renew_delay,
            renewal_calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn with_renew_error(checkpoint: Option<Checkpoint>, renew_error: &'static str) -> Self {
        Self {
            checkpoint,
            renew_result: false,
            renew_error: Some(renew_error),
            renew_delay: Duration::ZERO,
            renewal_calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl StateStore for StartupClaimViewStore {
    fn create_checkpoint(&self, _checkpoint: Checkpoint) -> std::io::Result<bool> {
        unreachable!("startup claim view only supports load and renew")
    }

    fn save_checkpoint(&self, _checkpoint: Checkpoint) -> std::io::Result<()> {
        unreachable!("startup claim view only supports load and renew")
    }

    fn load_checkpoint(&self, _task_id: &str) -> std::io::Result<Option<Checkpoint>> {
        Ok(self.checkpoint.clone())
    }

    fn claim_checkpoint(
        &self,
        _task_id: &str,
        _cycle_index: u32,
        _claim_token: &str,
        _lease_expires_at_ms: u64,
        _now_ms: u64,
    ) -> std::io::Result<Option<Checkpoint>> {
        unreachable!("startup claim view only supports load and renew")
    }

    fn commit_checkpoint(
        &self,
        _checkpoint: Checkpoint,
        _claim_token: &str,
        _expected_revision: u64,
    ) -> std::io::Result<bool> {
        unreachable!("startup claim view only supports load and renew")
    }

    fn renew_checkpoint_claim(
        &self,
        _task_id: &str,
        _claim_token: &str,
        _expected_revision: u64,
        _lease_expires_at_ms: u64,
        _now_ms: u64,
    ) -> std::io::Result<bool> {
        self.renewal_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(message) = self.renew_error {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, message));
        }
        std::thread::sleep(self.renew_delay);
        Ok(self.renew_result)
    }

    fn finalize_checkpoint(
        &self,
        _checkpoint: Checkpoint,
        _expected_revision: u64,
    ) -> std::io::Result<bool> {
        unreachable!("startup claim view only supports load and renew")
    }

    fn delete_checkpoint(&self, _task_id: &str) -> std::io::Result<()> {
        unreachable!("startup claim view only supports load and renew")
    }

    fn acknowledge_terminal(
        &self,
        _task_id: &str,
        _expected_revision: u64,
    ) -> std::io::Result<bool> {
        unreachable!("startup claim view only supports load and renew")
    }

    fn list_checkpoints(&self) -> std::io::Result<Vec<String>> {
        unreachable!("startup claim view only supports load and renew")
    }

    fn state_store_spec(&self) -> Option<crate::runtime::state::StateStoreSpec> {
        None
    }
}

#[test]
fn heartbeat_renews_before_operation_starts() {
    let case = worker_case("initial_renewal_precedes_operation");
    let task = AgentTask::new("heartbeat-ready", "model", "system", "prompt");
    let (store, claimed, initial_expiry) = claimed_checkpoint(&task, 10_000);
    let envelope = envelope(task.clone(), 30_000);
    let store_for_operation = store.clone();

    let renewed_before_operation =
        run_with_lease_heartbeat(store, &envelope, "owner", claimed.revision, move |_| {
            LeaseOperationResult::uncommitted(
                store_for_operation
                    .load_checkpoint(&task.task_id)
                    .expect("load checkpoint")
                    .expect("active checkpoint")
                    .lease_expires_at_ms
                    .is_some_and(|expiry| expiry > initial_expiry),
            )
        })
        .expect("heartbeat run");

    assert!(renewed_before_operation);
    assert_eq!(case["expected"]["operation_calls"].as_u64(), Some(1));
}

#[test]
fn initial_heartbeat_failure_prevents_operation() {
    let case = worker_case("initial_renewal_failure_has_no_side_effects");
    let task = AgentTask::new("heartbeat-failed", "model", "system", "prompt");
    let (_claimed_store, claimed, _) = claimed_checkpoint(&task, 30_000);
    let envelope = envelope(task, 30_000);
    let heartbeat_store = Arc::new(StartupClaimViewStore::with_renew_result(
        Some(claimed.clone()),
        false,
    ));
    let mut operation_called = false;

    let error = run_with_lease_heartbeat(
        heartbeat_store.clone(),
        &envelope,
        "owner",
        claimed.revision,
        |_| {
            operation_called = true;
            LeaseOperationResult::uncommitted(())
        },
    )
    .expect_err("initial renewal must fail");

    assert_eq!(error, case["expected"]["outcome"].as_str().unwrap());
    assert!(!operation_called);
    assert_eq!(
        heartbeat_store
            .renewal_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(case["expected"]["cycle_calls"].as_u64(), Some(0));
    assert_eq!(case["expected"]["model_calls"].as_u64(), Some(0));
    assert_eq!(case["expected"]["tool_calls"].as_u64(), Some(0));
    assert_eq!(case["expected"]["commit_calls"].as_u64(), Some(0));
}

#[test]
fn delayed_result_after_new_expiry_prevents_operation() {
    for renew_result in [true, false] {
        let task = AgentTask::new(
            format!("heartbeat-delayed-{renew_result}"),
            "model",
            "system",
            "prompt",
        );
        let (_claimed_store, claimed, _) = claimed_checkpoint(&task, 30_000);
        let envelope = envelope(task, 1);
        let heartbeat_store = Arc::new(StartupClaimViewStore::with_delayed_renewal(
            Some(claimed.clone()),
            Duration::from_millis(20),
            renew_result,
        ));
        let mut operation_called = false;

        let error = run_with_lease_heartbeat(
            heartbeat_store,
            &envelope,
            "owner",
            claimed.revision,
            |_| {
                operation_called = true;
                LeaseOperationResult::uncommitted(())
            },
        )
        .expect_err("expired renewal response must fail");

        assert_eq!(
            error,
            "checkpoint lease heartbeat failed: claim lease expired"
        );
        assert!(!operation_called);
    }
}

#[test]
fn delayed_result_after_known_expiry_prevents_operation() {
    for renew_result in [true, false] {
        let task = AgentTask::new(
            format!("heartbeat-known-expiry-{renew_result}"),
            "model",
            "system",
            "prompt",
        );
        let (_claimed_store, claimed, _) = claimed_checkpoint(&task, 1);
        let envelope = envelope(task, 30_000);
        let heartbeat_store = Arc::new(StartupClaimViewStore::with_delayed_renewal(
            Some(claimed.clone()),
            Duration::from_millis(20),
            renew_result,
        ));
        let mut operation_called = false;

        let error = run_with_lease_heartbeat(
            heartbeat_store,
            &envelope,
            "owner",
            claimed.revision,
            |_| {
                operation_called = true;
                LeaseOperationResult::uncommitted(())
            },
        )
        .expect_err("a response after the known lease expiry must fail");

        assert_eq!(
            error,
            "checkpoint lease heartbeat failed: claim lease expired"
        );
        assert!(!operation_called);
    }
}

#[test]
fn store_reported_expiry_maps_to_claim_lease_expired() {
    let task = AgentTask::new("heartbeat-store-expiry", "model", "system", "prompt");
    let (_claimed_store, claimed, known_expiry) = claimed_checkpoint(&task, 30_000);
    let store =
        StartupClaimViewStore::with_renew_error(Some(claimed.clone()), "claim lease expired");
    let request = prepare_lease_renewal(&task.task_id, 30_000, None)
        .unwrap_or_else(|failure| panic!("renewal request failed: {}", failure.message));

    let error = renew_checkpoint_lease(
        &store,
        &task.task_id,
        "owner",
        claimed.revision,
        request,
        known_expiry,
    )
    .err()
    .expect("store-reported expiry must remain typed");

    assert_eq!(error.kind, LeaseRenewalFailureKind::ClaimLeaseExpired);
    assert_eq!(error.message, "claim lease expired");
}

#[test]
fn heartbeat_start_rejects_invisible_or_mismatched_claim_before_renewal() {
    let task = AgentTask::new("heartbeat-startup-view", "model", "system", "prompt");
    let (_claimed_store, claimed, _) = claimed_checkpoint(&task, 30_000);
    let envelope = envelope(task, 30_000);
    let expected_revision = claimed.revision;

    let mut wrong_revision = claimed.clone();
    wrong_revision.revision += 1;
    let mut wrong_token = claimed.clone();
    wrong_token.claim_token = Some("other-owner".to_string());
    let mut wrong_cycle = claimed.clone();
    wrong_cycle.claimed_cycle = Some(2);
    let mut missing_expiry = claimed;
    missing_expiry.lease_expires_at_ms = None;

    for (name, visible_checkpoint) in [
        ("not visible", None),
        ("wrong revision", Some(wrong_revision)),
        ("wrong token", Some(wrong_token)),
        ("wrong cycle", Some(wrong_cycle)),
        ("missing expiry", Some(missing_expiry)),
    ] {
        let heartbeat_store = Arc::new(StartupClaimViewStore::new(visible_checkpoint));
        let mut operation_called = false;
        let error = run_with_lease_heartbeat(
            heartbeat_store.clone(),
            &envelope,
            "owner",
            expected_revision,
            |_| {
                operation_called = true;
                LeaseOperationResult::uncommitted(())
            },
        )
        .unwrap_err();

        assert_eq!(
            error, "checkpoint lease heartbeat failed: claim is no longer active",
            "{name}"
        );
        assert!(!operation_called, "{name}");
        assert_eq!(
            heartbeat_store
                .renewal_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0,
            "{name}"
        );
    }
}

#[test]
fn operation_panic_stops_heartbeat_before_unwinding() {
    let case = worker_case("operation_unwind_stops_heartbeat");
    let task = AgentTask::new("heartbeat-panic", "model", "system", "prompt");
    let (store, claimed, _) = claimed_checkpoint(&task, 30_000);
    let envelope = envelope(task, 30_000);

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = run_with_lease_heartbeat(
            store,
            &envelope,
            "owner",
            claimed.revision,
            |_| -> LeaseOperationResult<()> { panic!("operation panic") },
        );
    }));

    assert!(panic.is_err());
    assert_eq!(case["expected"]["renewals_after_stop"].as_u64(), Some(0));
    assert_eq!(case["expected"]["commit_calls"].as_u64(), Some(0));
    assert_eq!(case["expected"]["outcome"].as_str(), Some("unwind"));
}

#[test]
fn heartbeat_renews_until_commit_completes() {
    let case = worker_case("commit_barrier_keeps_heartbeat_active");
    let expected = &case["expected"];
    let task = AgentTask::new("heartbeat-commit", "model", "system", "prompt");
    let task_id = task.task_id.clone();
    let (store, claimed, _) = claimed_checkpoint(&task, 30_000);
    let envelope = envelope(task, 3_000);
    let expected_revision = claimed.revision;
    let worker_store = store.clone();
    let (commit_started_tx, commit_started_rx) = std::sync::mpsc::channel();
    let (release_commit_tx, release_commit_rx) = std::sync::mpsc::channel();

    let worker = std::thread::spawn(move || {
        run_with_lease_heartbeat(
            worker_store.clone(),
            &envelope,
            "owner",
            expected_revision,
            |heartbeat_status| {
                heartbeat_status.begin_commit().expect("healthy heartbeat");
                commit_started_tx.send(()).expect("signal commit phase");
                release_commit_rx
                    .recv_timeout(Duration::from_secs(30))
                    .expect("release commit phase");
                let mut checkpoint = worker_store
                    .load_checkpoint(&task_id)
                    .expect("load checkpoint")
                    .expect("active checkpoint");
                checkpoint.cycle_index = 1;
                let committed = worker_store
                    .commit_checkpoint(checkpoint, "owner", expected_revision)
                    .expect("commit checkpoint");
                if committed {
                    heartbeat_status
                        .mark_commit_succeeded()
                        .expect("mark durable commit");
                }
                LeaseOperationResult::new(committed, committed)
            },
        )
    });

    commit_started_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("commit phase started");
    let expiry_before_periodic_renewal = store
        .load_checkpoint("heartbeat-commit")
        .expect("load checkpoint")
        .expect("active checkpoint")
        .lease_expires_at_ms
        .expect("active lease");
    let deadline = Instant::now() + Duration::from_secs(30);
    let expiry_after_periodic_renewal = loop {
        let expiry = store
            .load_checkpoint("heartbeat-commit")
            .expect("load checkpoint")
            .expect("active checkpoint")
            .lease_expires_at_ms
            .expect("active lease");
        if expiry > expiry_before_periodic_renewal {
            break expiry;
        }
        assert!(
            Instant::now() < deadline,
            "heartbeat did not renew during commit"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(
        expected["periodic_renewals_during_commit_min"].as_u64(),
        Some(1)
    );
    let claim_error = store
        .claim_checkpoint(
            "heartbeat-commit",
            1,
            "contender",
            expiry_after_periodic_renewal + 3_000,
            expiry_before_periodic_renewal,
        )
        .expect_err("active commit lease must not be stolen");
    assert!(claim_error.to_string().contains("already claimed"));

    release_commit_tx.send(()).expect("release commit");
    assert!(worker
        .join()
        .expect("worker thread")
        .expect("heartbeat run"));
    assert_eq!(expected["contender_claimed"].as_bool(), Some(false));
    assert_eq!(expected["commit_calls"].as_u64(), Some(1));
    assert_eq!(expected["outcome"].as_str(), Some("success"));
}

#[test]
fn successful_commit_suppresses_claim_consumed_renewal_error() {
    let case = worker_case("successful_commit_beats_inflight_renewal_rejection");
    let task = AgentTask::new("heartbeat-commit-race", "model", "system", "prompt");
    let task_id = task.task_id.clone();
    let (store, claimed, _) = claimed_checkpoint(&task, 3_000);
    let envelope = envelope(task, 3_000);
    let expected_revision = claimed.revision;
    let operation_store = store.clone();
    let (heartbeat_store, periodic_started, release_periodic) = BlockingRenewStateStore::new(store);

    let result = run_with_lease_heartbeat(
        heartbeat_store,
        &envelope,
        "owner",
        expected_revision,
        move |heartbeat_status| {
            heartbeat_status.begin_commit().expect("healthy heartbeat");
            periodic_started
                .recv_timeout(Duration::from_secs(30))
                .expect("renewal started during commit");
            let mut checkpoint = operation_store
                .load_checkpoint(&task_id)
                .expect("load checkpoint")
                .expect("active checkpoint");
            checkpoint.cycle_index = 1;
            let committed = operation_store
                .commit_checkpoint(checkpoint, "owner", expected_revision)
                .expect("commit checkpoint");
            assert!(committed);
            heartbeat_status
                .mark_commit_succeeded()
                .expect("mark durable commit");
            release_periodic.send(()).expect("release rejected renewal");
            LeaseOperationResult::new("committed", committed)
        },
    )
    .expect("durable commit wins");

    assert_eq!(result, "committed");
    assert_eq!(case["expected"]["durable_commit"].as_bool(), Some(true));
    assert_eq!(
        case["expected"]["heartbeat_error_suppressed"].as_bool(),
        Some(true)
    );
    assert_eq!(case["expected"]["outcome"].as_str(), Some("success"));
}

#[test]
fn renewal_started_before_commit_is_not_suppressed_after_durable_commit() {
    let task = AgentTask::new("heartbeat-precommit-race", "model", "system", "prompt");
    let task_id = task.task_id.clone();
    let (store, claimed, _) = claimed_checkpoint(&task, 3_000);
    let envelope = envelope(task, 3_000);
    let expected_revision = claimed.revision;
    let operation_store = store.clone();
    let assertion_store = store.clone();
    let (heartbeat_store, periodic_started, release_periodic) = BlockingRenewStateStore::new(store);

    let error = run_with_lease_heartbeat(
        heartbeat_store,
        &envelope,
        "owner",
        expected_revision,
        move |heartbeat_status| {
            periodic_started
                .recv_timeout(Duration::from_secs(30))
                .expect("renewal started before commit");
            heartbeat_status.begin_commit().expect("healthy heartbeat");
            let mut checkpoint = operation_store
                .load_checkpoint(&task_id)
                .expect("load checkpoint")
                .expect("active checkpoint");
            checkpoint.cycle_index = 1;
            let committed = operation_store
                .commit_checkpoint(checkpoint, "owner", expected_revision)
                .expect("commit checkpoint");
            assert!(committed);
            heartbeat_status
                .mark_commit_succeeded()
                .expect("mark durable commit");
            release_periodic.send(()).expect("release rejected renewal");
            LeaseOperationResult::new("committed", committed)
        },
    )
    .expect_err("pre-commit renewal failure remains a coordination failure");

    assert_eq!(
        error,
        "checkpoint lease heartbeat failed: claim is no longer active"
    );
    let checkpoint = assertion_store
        .load_checkpoint("heartbeat-precommit-race")
        .expect("load committed checkpoint")
        .expect("durable checkpoint");
    assert_eq!(checkpoint.revision, expected_revision + 1);
    assert!(checkpoint.claim_token.is_none());
}

#[test]
fn expired_renewal_during_blocked_commit_is_never_suppressed() {
    let task = AgentTask::new("heartbeat-expired-commit", "model", "system", "prompt");
    let task_id = task.task_id.clone();
    let (store, claimed, _) = claimed_checkpoint(&task, 3_000);
    let envelope = envelope(task, 3_000);
    let expected_revision = claimed.revision;
    let operation_store = store.clone();
    let assertion_store = store.clone();
    let (heartbeat_store, periodic_started, release_periodic) = BlockingRenewStateStore::new(store);
    let (commit_started_tx, commit_started_rx) = std::sync::mpsc::channel();
    let (release_commit_tx, release_commit_rx) = std::sync::mpsc::channel();
    let (commit_succeeded_tx, commit_succeeded_rx) = std::sync::mpsc::channel();
    let (heartbeat_status_tx, heartbeat_status_rx) = std::sync::mpsc::channel();

    let worker = std::thread::spawn(move || {
        run_with_lease_heartbeat(
            heartbeat_store,
            &envelope,
            "owner",
            expected_revision,
            move |heartbeat_status| {
                heartbeat_status.begin_commit().expect("healthy heartbeat");
                heartbeat_status_tx
                    .send(heartbeat_status.clone())
                    .expect("share heartbeat status");
                commit_started_tx.send(()).expect("signal blocked commit");
                release_commit_rx
                    .recv_timeout(Duration::from_secs(30))
                    .expect("release blocked commit");
                let mut checkpoint = operation_store
                    .load_checkpoint(&task_id)
                    .expect("load checkpoint")
                    .expect("active checkpoint");
                checkpoint.cycle_index = 1;
                let committed = operation_store
                    .commit_checkpoint(checkpoint, "owner", expected_revision)
                    .expect("commit checkpoint");
                assert!(committed);
                heartbeat_status
                    .mark_commit_succeeded()
                    .expect("mark durable commit");
                commit_succeeded_tx.send(()).expect("signal durable commit");
                LeaseOperationResult::new("committed", committed)
            },
        )
    });

    commit_started_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("commit phase started");
    let heartbeat_status = heartbeat_status_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("heartbeat status shared");
    periodic_started
        .recv_timeout(Duration::from_secs(30))
        .expect("renewal started during blocked commit");
    let known_expiry = assertion_store
        .load_checkpoint("heartbeat-expired-commit")
        .expect("load renewed checkpoint")
        .expect("active checkpoint")
        .lease_expires_at_ms
        .expect("known lease expiry");
    assert!(
        now_unix_ms().expect("renewal call start observation") < known_expiry,
        "renewal must enter the store before the known lease expires"
    );
    let expiry_wait_deadline = Instant::now() + Duration::from_secs(30);
    while now_unix_ms().expect("current time") < known_expiry {
        assert!(
            Instant::now() < expiry_wait_deadline,
            "known lease did not expire"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    release_periodic.send(()).expect("release expired renewal");
    assert!(
        heartbeat_status.wait_for_failure(Duration::from_secs(30)),
        "expired renewal failure was not recorded"
    );
    {
        let state = heartbeat_status
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let failure = state.failure.as_ref().expect("recorded renewal failure");
        assert_eq!(
            failure.renewal.kind,
            LeaseRenewalFailureKind::ClaimLeaseExpired
        );
        assert!(failure.renewal_started_during_commit);
    }
    release_commit_tx.send(()).expect("release commit");
    commit_succeeded_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("durable commit succeeded");

    let error = worker
        .join()
        .expect("worker thread")
        .expect_err("expired claim failure cannot be suppressed");
    assert_eq!(
        error,
        "checkpoint lease heartbeat failed: claim lease expired"
    );
    let checkpoint = assertion_store
        .load_checkpoint("heartbeat-expired-commit")
        .expect("load committed checkpoint")
        .expect("durable checkpoint");
    assert_eq!(checkpoint.revision, expected_revision + 1);
    assert!(checkpoint.claim_token.is_none());
}

#[test]
fn heartbeat_interval_is_shorter_than_every_positive_lease() {
    for lease_duration_ms in lease_lifecycle()["interval_lease_ms_cases"]
        .as_array()
        .expect("interval cases")
        .iter()
        .map(|value| value.as_u64().expect("lease duration"))
    {
        assert!(
            lease_heartbeat_interval(lease_duration_ms) < Duration::from_millis(lease_duration_ms)
        );
    }
}

#[test]
fn deadline_clamped_lease_drives_heartbeat_interval() {
    let case = &lease_lifecycle()["expiry_cases"][0];
    let now_ms = case["now_ms"].as_u64().expect("now");
    let expiry_ms = lease_expiry_at(
        now_ms,
        case["lease_duration_ms"].as_u64().expect("lease duration"),
        case["deadline_unix_ms"].as_u64(),
    )
    .expect("deadline-clamped expiry");
    let effective_lease_ms = expiry_ms - now_ms;

    assert!(
        lease_heartbeat_interval(effective_lease_ms) < Duration::from_millis(effective_lease_ms)
    );
}

#[test]
fn lease_expiry_cases_match_shared_contract() {
    for case in lease_lifecycle()["expiry_cases"]
        .as_array()
        .expect("expiry cases")
    {
        let result = lease_expiry_at(
            case["now_ms"].as_u64().expect("now"),
            case["lease_duration_ms"].as_u64().expect("lease duration"),
            case["deadline_unix_ms"].as_u64(),
        );
        match case["expected_error"].as_str() {
            Some(expected_error) => assert_eq!(result.unwrap_err(), expected_error),
            None => assert_eq!(
                result.unwrap(),
                case["expected_expiry_ms"]
                    .as_u64()
                    .expect("expected expiry")
            ),
        }
    }
}
