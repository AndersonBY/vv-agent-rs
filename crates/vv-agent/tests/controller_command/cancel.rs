use super::*;

fn wall_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("wall clock")
        .as_millis() as u64
}

#[test]
fn controller_cancel_is_terminal_and_never_fabricates_a_second_transition() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint = minimal_checkpoint();
    let key = checkpoint.checkpoint_key.clone();
    let handle =
        ControllerHandle::new(&key, &checkpoint.root_run_id, &checkpoint.trace_id).expect("handle");
    store.create_checkpoint(checkpoint).expect("create");
    let cancel = ControllerCommand::new(
        "command-cancel",
        handle.clone(),
        1,
        0,
        ControllerCommandVariant::Cancel,
    )
    .expect("cancel");
    store
        .resolve_controller_command(cancel)
        .expect("cancel command");
    let terminal = store
        .load_checkpoint(&key)
        .expect("load")
        .expect("checkpoint");
    assert_eq!(terminal.status, vv_agent::CheckpointStatus::Failed);
    assert!(terminal.terminal_result.is_some());
    let stale = ControllerCommand::new(
        "command-cancel-stale",
        handle,
        1,
        0,
        ControllerCommandVariant::Resume,
    )
    .expect("stale command");
    assert!(matches!(
        store
            .resolve_controller_command(stale)
            .expect("terminal command is a closed rejected resolution"),
        ControllerCommandResolution::Rejected { error }
            if error.starts_with("controller_command_stale:")
    ));
    assert_eq!(
        store
            .load_checkpoint(&key)
            .expect("reload")
            .expect("checkpoint")
            .revision,
        1
    );
}

#[test]
fn controller_cancel_sets_signal_without_stealing_live_memory_claim() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint = minimal_checkpoint();
    let key = checkpoint.checkpoint_key.clone();
    let handle =
        ControllerHandle::new(&key, &checkpoint.root_run_id, &checkpoint.trace_id).expect("handle");
    store.create_checkpoint(checkpoint).expect("create");
    let now = wall_time_ms();
    let claimed = store
        .claim_checkpoint(&key, 1, "owner-a", now + 60_000, now, ClaimMode::Continue)
        .expect("claim")
        .expect("claimed");
    let command = ControllerCommand::new(
        "command-cancel-live-memory",
        handle,
        claimed.resume_attempt,
        claimed.revision,
        ControllerCommandVariant::Cancel,
    )
    .expect("cancel command");
    let replay = command.clone();
    let applied = store
        .resolve_controller_command(command)
        .expect("live cancellation");
    assert!(matches!(
        applied,
        ControllerCommandResolution::Applied { .. }
    ));
    let checkpoint = store
        .load_checkpoint(&key)
        .expect("load")
        .expect("checkpoint");
    assert_eq!(checkpoint.revision, claimed.revision);
    assert_eq!(checkpoint.claim_token.as_deref(), Some("owner-a"));
    assert!(checkpoint.cancel_requested);
    assert_eq!(checkpoint.event_outbox.len(), 1);
    assert_eq!(
        checkpoint.event_outbox[0].event["cancel_requested"],
        json!({"from": false, "to": true})
    );
    assert!(matches!(
        store
            .resolve_controller_command(replay)
            .expect("same command replay"),
        ControllerCommandResolution::Replayed { .. }
    ));
    assert_eq!(
        store
            .load_checkpoint(&key)
            .expect("reload")
            .expect("checkpoint")
            .event_outbox
            .len(),
        1
    );
    assert!(matches!(
        store
        .renew_checkpoint_claim(&key, "owner-a", now + 120_000, now + 1)
        .expect("renew"),
        vv_agent::CheckpointRenewalOutcome::CancelRequested {
            lease_expires_at_ms: value
        }
        if value == now + 120_000
    ));
}

#[test]
fn controller_suspend_resume_is_fenced_and_replayable() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint = minimal_checkpoint();
    let key = checkpoint.checkpoint_key.clone();
    let handle =
        ControllerHandle::new(&key, &checkpoint.root_run_id, &checkpoint.trace_id).expect("handle");
    store.create_checkpoint(checkpoint).expect("create");
    let suspend = ControllerCommand::new(
        "command-suspend",
        handle.clone(),
        1,
        0,
        ControllerCommandVariant::Suspend,
    )
    .expect("suspend");
    let suspended = store
        .resolve_controller_command(suspend.clone())
        .expect("suspend command");
    assert_eq!(suspended.kind(), "applied");
    assert_eq!(
        store
            .load_checkpoint(&key)
            .expect("load")
            .expect("checkpoint")
            .status,
        vv_agent::CheckpointStatus::Suspended
    );
    assert_eq!(
        store
            .resolve_controller_command(suspend)
            .expect("suspend replay")
            .kind(),
        "replayed"
    );
    let resume = ControllerCommand::new(
        "command-resume",
        handle,
        1,
        1,
        ControllerCommandVariant::Resume,
    )
    .expect("resume");
    let resumed = store
        .resolve_controller_command(resume)
        .expect("resume command");
    match resumed {
        ControllerCommandResolution::Applied { wake, .. } => {
            assert_eq!(wake.action, "recovery_dispatch");
            assert_eq!(wake.logical_cycle, 1);
        }
        other => panic!("unexpected resolution: {other:?}"),
    }
    assert_eq!(
        store
            .load_checkpoint(&key)
            .expect("load")
            .expect("checkpoint")
            .status,
        vv_agent::CheckpointStatus::Running
    );
}

#[test]
fn distinct_live_cancel_after_signal_is_an_applied_noop_memory() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint = minimal_checkpoint();
    let key = checkpoint.checkpoint_key.clone();
    let handle =
        ControllerHandle::new(&key, &checkpoint.root_run_id, &checkpoint.trace_id).expect("handle");
    store.create_checkpoint(checkpoint).expect("create");
    let now = wall_time_ms();
    let claimed = store
        .claim_checkpoint(&key, 1, "owner-a", now + 60_000, now, ClaimMode::Continue)
        .expect("claim")
        .expect("claimed");
    store
        .resolve_controller_command(
            ControllerCommand::new(
                "command-cancel-first",
                handle.clone(),
                claimed.resume_attempt,
                claimed.revision,
                ControllerCommandVariant::Cancel,
            )
            .expect("first cancel"),
        )
        .expect("first cancellation");
    let before = store
        .load_checkpoint(&key)
        .expect("load before no-op")
        .expect("checkpoint before no-op");
    let resolution = store
        .resolve_controller_command(
            ControllerCommand::new(
                "command-cancel-second",
                handle,
                before.resume_attempt,
                before.revision,
                ControllerCommandVariant::Cancel,
            )
            .expect("second cancel"),
        )
        .expect("distinct cancellation no-op");
    let receipt = match resolution {
        ControllerCommandResolution::Applied { receipt, wake } => {
            assert_eq!(wake.action, "none");
            receipt
        }
        other => panic!("expected applied no-op receipt, got {other:?}"),
    };
    assert_eq!(receipt.resulting_revision, before.revision);
    assert_eq!(receipt.resulting_status, "running");
    let after = store
        .load_checkpoint(&key)
        .expect("load after no-op")
        .expect("checkpoint after no-op");
    assert_eq!(after, before);
}

#[test]
fn controller_cancel_sets_signal_without_stealing_live_sqlite_claim() {
    let directory = tempdir().expect("tempdir");
    let store = SqliteCheckpointStore::new(directory.path().join("cancel-live.sqlite"))
        .expect("open sqlite");
    let checkpoint = minimal_checkpoint();
    let key = checkpoint.checkpoint_key.clone();
    let handle =
        ControllerHandle::new(&key, &checkpoint.root_run_id, &checkpoint.trace_id).expect("handle");
    store.create_checkpoint(checkpoint).expect("create");
    let now = wall_time_ms();
    let claimed = store
        .claim_checkpoint(&key, 1, "owner-a", now + 60_000, now, ClaimMode::Continue)
        .expect("claim")
        .expect("claimed");
    let command = ControllerCommand::new(
        "command-cancel-live-sqlite",
        handle.clone(),
        claimed.resume_attempt,
        claimed.revision,
        ControllerCommandVariant::Cancel,
    )
    .expect("cancel command");
    store
        .resolve_controller_command(command)
        .expect("live cancellation");
    let checkpoint = store
        .load_checkpoint(&key)
        .expect("load")
        .expect("checkpoint");
    assert_eq!(checkpoint.revision, claimed.revision);
    assert_eq!(checkpoint.claim_token.as_deref(), Some("owner-a"));
    assert!(checkpoint.cancel_requested);
    assert_eq!(checkpoint.event_outbox.len(), 1);
    assert_eq!(
        checkpoint.event_outbox[0].event["cancel_requested"],
        json!({"from": false, "to": true})
    );

    let second_before = checkpoint.clone();
    let second = ControllerCommand::new(
        "command-cancel-live-sqlite-second",
        handle,
        second_before.resume_attempt,
        second_before.revision,
        ControllerCommandVariant::Cancel,
    )
    .expect("second cancel command");
    let resolution = store
        .resolve_controller_command(second)
        .expect("distinct live cancellation no-op");
    match resolution {
        ControllerCommandResolution::Applied { receipt, wake } => {
            assert_eq!(wake.action, "none");
            assert_eq!(receipt.resulting_revision, second_before.revision);
            assert_eq!(receipt.resulting_status, "running");
        }
        other => panic!("expected applied no-op receipt, got {other:?}"),
    }
    assert_eq!(
        store
            .load_checkpoint(&key)
            .expect("reload after no-op")
            .expect("checkpoint after no-op"),
        second_before
    );
}

#[test]
fn expired_claim_cancel_reclaims_and_closes_memory_checkpoint() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint = minimal_checkpoint();
    let key = checkpoint.checkpoint_key.clone();
    let handle =
        ControllerHandle::new(&key, &checkpoint.root_run_id, &checkpoint.trace_id).expect("handle");
    store.create_checkpoint(checkpoint).expect("create");
    let claimed = store
        .claim_checkpoint(&key, 1, "expired-owner", 1, 0, ClaimMode::Continue)
        .expect("claim")
        .expect("claimed");
    let command = ControllerCommand::new(
        "command-cancel-expired-memory",
        handle,
        claimed.resume_attempt,
        claimed.revision,
        ControllerCommandVariant::Cancel,
    )
    .expect("cancel command");
    store
        .resolve_controller_command(command)
        .expect("expired cancellation");
    let terminal = store
        .load_checkpoint(&key)
        .expect("load")
        .expect("checkpoint");
    assert_eq!(terminal.status, vv_agent::CheckpointStatus::Failed);
    assert!(terminal.claim_token.is_none());
    assert_eq!(terminal.resume_attempt, claimed.resume_attempt + 1);
    assert!(terminal.cancel_requested);
    assert_eq!(terminal.revision, claimed.revision + 1);
    assert_eq!(
        terminal.terminal_result.as_ref().unwrap()["error"]["code"],
        "cancelled_with_unknown_outcome"
    );
    assert_eq!(
        terminal.terminal_result.as_ref().unwrap()["completion_reason"],
        "cancelled"
    );
    assert_eq!(
        terminal.terminal_result.as_ref().unwrap()["error"]["message"],
        "Operation was cancelled"
    );
    assert_eq!(
        terminal.terminal_result.as_ref().unwrap()["error"]["retryable"],
        false
    );
}

#[test]
fn expired_claim_suspend_releases_memory_claim() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint = minimal_checkpoint();
    let key = checkpoint.checkpoint_key.clone();
    let handle =
        ControllerHandle::new(&key, &checkpoint.root_run_id, &checkpoint.trace_id).expect("handle");
    store.create_checkpoint(checkpoint).expect("create");
    let claimed = store
        .claim_checkpoint(&key, 1, "expired-owner", 1, 0, ClaimMode::Continue)
        .expect("claim")
        .expect("claimed");
    let command = ControllerCommand::new(
        "command-suspend-expired-memory",
        handle,
        claimed.resume_attempt,
        claimed.revision,
        ControllerCommandVariant::Suspend,
    )
    .expect("suspend command");
    store
        .resolve_controller_command(command)
        .expect("expired suspension");
    let suspended = store
        .load_checkpoint(&key)
        .expect("load")
        .expect("checkpoint");
    assert_eq!(suspended.status, vv_agent::CheckpointStatus::Suspended);
    assert!(suspended.claim_token.is_none());
    assert!(suspended.suspended_origin.is_some());
}

#[test]
fn expired_claim_cancel_and_suspend_reclaim_sqlite_checkpoint() {
    let directory = tempdir().expect("tempdir");
    let store =
        SqliteCheckpointStore::new(directory.path().join("expired.sqlite")).expect("open sqlite");

    let checkpoint = minimal_checkpoint();
    let cancel_key = "checkpoint-expired-cancel-sqlite";
    let mut cancel_checkpoint = checkpoint.clone();
    cancel_checkpoint.checkpoint_key = cancel_key.to_string();
    let cancel_handle = ControllerHandle::new(
        cancel_key,
        &cancel_checkpoint.root_run_id,
        &cancel_checkpoint.trace_id,
    )
    .expect("cancel handle");
    store
        .create_checkpoint(cancel_checkpoint)
        .expect("create cancel checkpoint");
    let claimed = store
        .claim_checkpoint(
            cancel_key,
            1,
            "expired-cancel-owner",
            1,
            0,
            ClaimMode::Continue,
        )
        .expect("cancel claim")
        .expect("cancel claimed");
    store
        .resolve_controller_command(
            ControllerCommand::new(
                "command-cancel-expired-sqlite",
                cancel_handle,
                claimed.resume_attempt,
                claimed.revision,
                ControllerCommandVariant::Cancel,
            )
            .expect("cancel command"),
        )
        .expect("expired sqlite cancellation");
    let terminal = store
        .load_checkpoint(cancel_key)
        .expect("load cancel")
        .expect("cancel checkpoint");
    assert_eq!(terminal.status, vv_agent::CheckpointStatus::Failed);
    assert!(terminal.claim_token.is_none());
    assert_eq!(terminal.resume_attempt, claimed.resume_attempt + 1);
    assert!(terminal.cancel_requested);
    assert_eq!(
        terminal.terminal_result.as_ref().unwrap()["completion_reason"],
        "cancelled"
    );
    assert_eq!(
        terminal.terminal_result.as_ref().unwrap()["error"]["code"],
        "cancelled_with_unknown_outcome"
    );

    let suspend_key = "checkpoint-expired-suspend-sqlite";
    let mut suspend_checkpoint = checkpoint;
    suspend_checkpoint.checkpoint_key = suspend_key.to_string();
    let suspend_handle = ControllerHandle::new(
        suspend_key,
        &suspend_checkpoint.root_run_id,
        &suspend_checkpoint.trace_id,
    )
    .expect("suspend handle");
    store
        .create_checkpoint(suspend_checkpoint)
        .expect("create suspend checkpoint");
    let claimed = store
        .claim_checkpoint(
            suspend_key,
            1,
            "expired-suspend-owner",
            1,
            0,
            ClaimMode::Continue,
        )
        .expect("suspend claim")
        .expect("suspend claimed");
    store
        .resolve_controller_command(
            ControllerCommand::new(
                "command-suspend-expired-sqlite",
                suspend_handle,
                claimed.resume_attempt,
                claimed.revision,
                ControllerCommandVariant::Suspend,
            )
            .expect("suspend command"),
        )
        .expect("expired sqlite suspension");
    let suspended = store
        .load_checkpoint(suspend_key)
        .expect("load suspend")
        .expect("suspend checkpoint");
    assert_eq!(suspended.status, vv_agent::CheckpointStatus::Suspended);
    assert!(suspended.claim_token.is_none());
}
