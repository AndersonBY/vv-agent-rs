use std::sync::atomic::AtomicUsize;
use std::time::Duration;

use super::*;
use crate::InMemoryCheckpointStore;

#[test]
fn canonical_unknown_journal_recovers_once_through_controller() {
    let fixture: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/parity/operation_journal.json"
    )))
    .unwrap();
    let case = fixture["recovery_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "started_tool_surfaces_unknown_outcome_to_model_by_default")
        .unwrap();
    let expected = &case["expected"];
    let entries = fixture["valid_entries"].as_array().unwrap();
    let entry = entries
        .iter()
        .find(|entry| entry["name"] == case["entry"])
        .unwrap();
    let receipt = entries
        .iter()
        .find(|entry| entry["name"] == case["receipt_entry"])
        .unwrap();
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
    payload["checkpoint_key"] =
        fixture["receipt_identity"]["golden_identity"]["checkpoint_key"].clone();
    let seed = crate::runtime::checkpoint_codec::checkpoint_from_value(&payload, 262_144).unwrap();
    let store = Arc::new(InMemoryCheckpointStore::new());
    assert!(store.create_checkpoint(seed.clone()).unwrap());
    let mut claimed = store
        .claim_checkpoint(
            &seed.checkpoint_key,
            1,
            "expired-owner",
            200,
            100,
            ClaimMode::Continue,
        )
        .unwrap()
        .unwrap();
    claimed.tool_journal = vec![OperationJournalEntry::from_value(&entry["entry"]).unwrap()];
    let revision = claimed.revision;
    assert!(store
        .progress_checkpoint(claimed, "expired-owner", revision)
        .unwrap());
    let observed = Arc::new(Mutex::new(Vec::new()));
    let mut original_completion = None;
    for recovery in 0..2 {
        let before = store
            .load_checkpoint(&seed.checkpoint_key)
            .unwrap()
            .unwrap();
        std::thread::sleep(Duration::from_millis(
            before
                .lease_expires_at_ms
                .unwrap_or(0)
                .saturating_sub(now_ms().unwrap()),
        ));
        let sink = observed.clone();
        let mut controller = CheckpointResumeController::new(CheckpointControllerRequest {
            config: CheckpointConfig {
                store: Some(store.clone()),
                key: Some(seed.checkpoint_key.clone()),
                resume_policy: ResumePolicy::RequireExisting,
                ambiguous_tool_policy: serde_json::from_value(case["policy"].clone()).unwrap(),
                ..CheckpointConfig::default()
            },
            task_id: seed.task_id.clone(),
            run_id: seed.root_run_id.clone(),
            trace_id: seed.trace_id.clone(),
            agent_name: "canonical-recovery".to_string(),
            run_definition: seed.run_definition.clone(),
            run_definition_digest: seed.run_definition_digest.clone(),
            initial_messages: vec![],
            initial_shared_state: BTreeMap::new(),
            initial_budget_usage: None,
            extensions: vec![],
            reconciliation_provider: None,
            event_sink: Arc::new(move |event| {
                sink.lock()
                    .unwrap()
                    .push(serde_json::to_value(event).unwrap());
                Ok(())
            }),
            event_store: None,
            preloaded_checkpoint: None,
        })
        .unwrap();
        controller.set_lease_duration_ms(1000).unwrap();
        assert!(controller.admit().unwrap().is_none());
        assert!(controller.begin_cycle(1).unwrap().is_none());
        let retained = store
            .load_checkpoint(&seed.checkpoint_key)
            .unwrap()
            .unwrap();
        assert_eq!(retained.resume_attempt, before.resume_attempt + 1);
        assert_eq!(retained.tool_journal.len(), 1);
        let entry = &retained.tool_journal[0];
        assert_eq!(
            serde_json::to_value(entry.state).unwrap(),
            expected["persisted_state"]
        );
        assert_eq!(
            serde_json::to_value(&entry.result).unwrap(),
            receipt["entry"]["result"]
        );
        assert_eq!(
            serde_json::to_value(&entry.result_digest).unwrap(),
            expected["result_digest"]
        );
        assert_eq!(
            serde_json::to_value(&entry.resume_observation).unwrap(),
            expected["resume_observation"]
        );
        let completion: Vec<_> = retained
            .event_outbox
            .iter()
            .filter(|entry| entry.event["type"] == expected["event"])
            .collect();
        assert_eq!(completion.len(), 1);
        assert_eq!(
            completion[0].event_id,
            expected["event_id"].as_str().unwrap()
        );
        if recovery == 0 {
            original_completion = Some(completion[0].event.clone());
        } else {
            assert_eq!(Some(&completion[0].event), original_completion.as_ref());
        }
        controller.close();
    }
    assert_eq!(
        observed
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event["type"] == expected["event"])
            .count(),
        1
    );
}

#[test]
#[ignore = "requires a cross-runtime SQLite database"]
fn cross_runtime_sqlite_probe_from_environment() {
    let path = std::env::var("VV_AGENT_CROSS_RUNTIME_DB").expect("cross-runtime SQLite path");
    let mode = std::env::var("VV_AGENT_CROSS_RUNTIME_MODE").expect("cross-runtime mode");
    let store = Arc::new(crate::SqliteCheckpointStore::new(path).unwrap());
    let checkpoint = match mode.as_str() {
        "write_rust" => {
            let fixture: Value = serde_json::from_str(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/parity/checkpoint_codec.json"
            )))
            .unwrap();
            let mut payload = fixture["valid_cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|case| case["name"] == "minimal_running")
                .unwrap()["payload"]
                .clone();
            payload["checkpoint_key"] = json!("rust-wrote");
            let mut checkpoint =
                crate::runtime::checkpoint_codec::checkpoint_from_value(&payload, 262_144).unwrap();
            checkpoint.messages = vec![Message::user("from Rust")];
            checkpoint.shared_state = BTreeMap::from([
                ("format".to_string(), json!("checkpoint")),
                ("writer".to_string(), json!("rust")),
            ]);
            checkpoint
        }
        "read_python" => {
            let checkpoint = store
                .load_checkpoint("python-wrote")
                .unwrap()
                .expect("Python checkpoint");
            assert_eq!(checkpoint.messages, vec![Message::user("from Python")]);
            assert_eq!(
                checkpoint.shared_state,
                BTreeMap::from([
                    ("format".to_string(), json!("checkpoint")),
                    ("writer".to_string(), json!("python")),
                ])
            );
            assert_eq!(
                checkpoint.run_definition_digest,
                run_definition_digest(&checkpoint.run_definition).unwrap()
            );
            let entry = &checkpoint.tool_journal[0];
            assert_eq!(
                entry.idempotency_support,
                Some(ToolIdempotency::Unsupported)
            );
            assert!(entry.idempotency_key.is_none());
            entry.verify_request(&json!({
                "schema_version": "vv-agent.operation-request.v1", "kind": "tool",
                "request": {"tool_call_id": "cross-tool", "tool_name": "unsafe_write", "arguments": {}, "idempotency_key": null},
            })).unwrap();
            std::thread::sleep(Duration::from_millis(
                checkpoint
                    .lease_expires_at_ms
                    .unwrap_or(0)
                    .saturating_sub(now_ms().unwrap()),
            ));
            checkpoint
        }
        other => panic!("unknown cross-runtime mode: {other}"),
    };
    let mut controller = CheckpointResumeController::new(CheckpointControllerRequest {
        config: CheckpointConfig {
            store: Some(store.clone()),
            key: Some(checkpoint.checkpoint_key.clone()),
            resume_policy: if mode == "write_rust" {
                ResumePolicy::New
            } else {
                ResumePolicy::RequireExisting
            },
            ..CheckpointConfig::default()
        },
        task_id: checkpoint.task_id.clone(),
        run_id: checkpoint.root_run_id.clone(),
        trace_id: checkpoint.trace_id.clone(),
        agent_name: "cross-runtime".to_string(),
        run_definition: checkpoint.run_definition.clone(),
        run_definition_digest: checkpoint.run_definition_digest.clone(),
        initial_messages: checkpoint.messages.clone(),
        initial_shared_state: checkpoint.shared_state.clone(),
        initial_budget_usage: None,
        extensions: vec![],
        reconciliation_provider: None,
        event_sink: Arc::new(|_| Ok(())),
        event_store: None,
        preloaded_checkpoint: None,
    })
    .unwrap();
    controller.set_lease_duration_ms(1000).unwrap();
    assert!(controller.admit().unwrap().is_none());
    let (plan, interruption) = controller
        .plan_tool(
            1,
            &ToolCall::new("cross-tool", "unsafe_write", BTreeMap::new()),
            ToolIdempotency::Unsupported,
            None,
            None,
            None,
        )
        .unwrap();
    assert!(interruption.is_none());
    assert!(plan.idempotency_key.is_none());
    assert!(plan.replay_result.is_none());
    let retained = store
        .load_checkpoint(&checkpoint.checkpoint_key)
        .unwrap()
        .unwrap();
    assert_eq!(retained.tool_journal.len(), 1);
    assert!(retained.tool_journal[0].idempotency_key.is_none());
    if mode == "read_python" {
        let entry = &checkpoint.tool_journal[0];
        assert_eq!(
            plan.operation_id.as_deref(),
            Some(entry.operation_id.as_str())
        );
        assert_eq!(plan.attempt, Some(entry.attempt));
        assert_eq!(
            plan.request_digest.as_deref(),
            Some(entry.request_digest.as_str())
        );
        assert_eq!(retained.resume_attempt, checkpoint.resume_attempt + 1);
    }
    controller.close();
}

struct RecordingExtension {
    snapshots: Arc<AtomicUsize>,
}

impl CheckpointExtension for RecordingExtension {
    fn namespace(&self) -> &str {
        "test"
    }

    fn version(&self) -> &str {
        "1"
    }

    fn required(&self) -> bool {
        false
    }

    fn snapshot(&self) -> CheckpointResult<Value> {
        self.snapshots.fetch_add(1, Ordering::SeqCst);
        Ok(Value::Null)
    }

    fn restore(&self, _state: &Value) -> CheckpointResult<()> {
        Ok(())
    }
}

#[test]
fn transient_store_error_does_not_poison_heartbeat_retry() {
    let now = now_ms().expect("current time");
    let known_expiry = now + 10_000;

    assert_eq!(
        renew_heartbeat_once(
            |_, _| {
                Err(CheckpointError::new(
                    "test_transient_store_error",
                    "injected transient renewal failure",
                ))
            },
            1_000,
            known_expiry,
        )
        .expect("transient store error is retryable"),
        None
    );
    assert!(renew_heartbeat_once(
        |_, _| Ok(CheckpointRenewalOutcome::Renewed {
            lease_expires_at_ms: known_expiry
        }),
        1_000,
        known_expiry,
    )
    .expect("renewal retry")
    .is_some());

    let cancel_expiry = renew_heartbeat_once(
        |_, _| {
            Ok(CheckpointRenewalOutcome::CancelRequested {
                lease_expires_at_ms: known_expiry,
            })
        },
        1_000,
        known_expiry,
    )
    .expect("cancellation renewal keeps the claim alive")
    .expect("cancellation renewal returns its lease expiry");
    assert!(cancel_expiry > now);

    let false_error = renew_heartbeat_once(
        |_, _| Ok(CheckpointRenewalOutcome::ClaimLost { revision: 1 }),
        1_000,
        known_expiry,
    )
    .expect_err("false renewal must fail closed");
    assert_eq!(false_error.code(), "checkpoint_lease_lost");

    let expired_error = renew_heartbeat_once(
        |_, _| {
            Ok(CheckpointRenewalOutcome::Renewed {
                lease_expires_at_ms: 1,
            })
        },
        1_000,
        0,
    )
    .expect_err("expired lease must fail closed");
    assert_eq!(expired_error.code(), "checkpoint_lease_lost");
}

#[test]
fn external_dispatch_rejects_renewal_returned_after_lease_expiry() {
    let now = now_ms().expect("current time");
    let error = renew_heartbeat_once(
        |_, _| {
            std::thread::sleep(Duration::from_millis(100));
            Ok(CheckpointRenewalOutcome::Renewed {
                lease_expires_at_ms: 1,
            })
        },
        1_000,
        now + 50,
    )
    .expect_err("delayed renewal must fail closed");
    assert_eq!(error.code(), "checkpoint_lease_lost");
}

#[test]
fn progress_checks_heartbeat_before_snapshotting_extensions() {
    let snapshots = Arc::new(AtomicUsize::new(0));
    let mut controller = CheckpointResumeController::new(CheckpointControllerRequest {
        config: CheckpointConfig::with_store(InMemoryCheckpointStore::new()),
        task_id: "task".to_string(),
        run_id: "run".to_string(),
        trace_id: "trace".to_string(),
        agent_name: "agent".to_string(),
        run_definition: json!({}),
        run_definition_digest: String::new(),
        initial_messages: Vec::new(),
        initial_shared_state: BTreeMap::new(),
        initial_budget_usage: None,
        extensions: vec![Arc::new(RecordingExtension {
            snapshots: snapshots.clone(),
        })],
        reconciliation_provider: None,
        event_sink: Arc::new(|_| Ok(())),
        event_store: None,
        preloaded_checkpoint: None,
    })
    .expect("controller");
    controller.checkpoint = Some(Checkpoint::default());
    let (stop, _stopped) = mpsc::channel();
    controller.heartbeat = Some(HeartbeatHandle {
        stop,
        error: Arc::new(Mutex::new(Some(CheckpointError::new(
            "checkpoint_lease_lost",
            "injected heartbeat failure",
        )))),
        lease_expires_at_ms: Arc::new(AtomicU64::new(0)),
        thread: None,
    });

    let error = controller
        .progress()
        .expect_err("failed heartbeat must stop progress");
    assert_eq!(error.code(), "checkpoint_lease_lost");
    assert_eq!(snapshots.load(Ordering::SeqCst), 0);
}
