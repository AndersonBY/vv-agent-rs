use super::*;

fn create_recovery_wake(
    store: &dyn CheckpointStore,
    checkpoint_key: &str,
    command_id: &str,
) -> ControllerCommand {
    let mut checkpoint = minimal_checkpoint();
    checkpoint.checkpoint_key = checkpoint_key.to_string();
    let run_id = checkpoint.root_run_id.clone();
    let trace_id = checkpoint.trace_id.clone();
    store
        .create_checkpoint(checkpoint)
        .expect("create wake checkpoint");
    let claimed = store
        .claim_checkpoint(
            checkpoint_key,
            1,
            "wake-worker",
            1_000_000,
            0,
            ClaimMode::Continue,
        )
        .expect("claim wake checkpoint")
        .expect("wake checkpoint claim");
    let request = HostInteractionRequest::new(
        format!("{command_id}-interaction"),
        1,
        format!("{command_id}-operation"),
        format!("{command_id}-tool"),
        "Choose.",
    )
    .expect("wake request");
    let admitted = store
        .produce_host_interaction(
            request.clone(),
            &HostInteractionAdmissionContext::new(
                checkpoint_key,
                claimed.revision,
                "wake-worker",
                1,
                0,
                claimed.lease_expires_at_ms.expect("wake lease"),
            )
            .expect("wake admission context"),
        )
        .expect("admit wake request");
    let command = ControllerCommand::new(
        command_id,
        ControllerHandle::new(checkpoint_key, &run_id, &trace_id).expect("wake handle"),
        1,
        admitted.checkpoint_revision,
        ControllerCommandVariant::HostInteractionResponse {
            interaction_id: request.interaction_id.clone(),
            logical_cycle: request.logical_cycle,
            operation_id: request.operation_id.clone(),
            tool_call_id: request.tool_call_id.clone(),
            request_digest: request.request_digest.clone(),
            response: HostInteractionMessage::user("approved").expect("wake response"),
        },
    )
    .expect("wake command");
    store
        .resolve_controller_command(command.clone())
        .expect("resolve wake command");
    command
}
fn assert_full_reaper_record(store: &dyn CheckpointStore, checkpoint_key: &str, command_id: &str) {
    let command = create_recovery_wake(store, checkpoint_key, command_id);
    assert!(store
        .reap_controller_command_wakes("foreign-checkpoint", 0)
        .expect("scope reaper")
        .is_empty());
    let initial = store
        .reap_controller_command_wakes(checkpoint_key, 0)
        .expect("pending reaper");
    assert_eq!(initial.len(), 1);
    let record = &initial[0];
    assert_eq!(record.command_id, command.command_id);
    assert_eq!(record.command_digest, command.command_digest);
    assert_eq!(record.checkpoint_key, checkpoint_key);
    assert_eq!(record.handle, command.handle);
    assert_eq!(record.resume_attempt, command.resume_attempt);
    assert_eq!(record.expected_revision, command.expected_revision);
    assert_eq!(record.resulting_revision, command.expected_revision + 1);
    assert_eq!(record.resulting_status, "running");
    assert_eq!(
        record.outbox_id,
        controller_receipt_outbox_id(&command.command_id, &command.command_digest)
            .expect("wake outbox id")
    );
    assert_eq!(record.outbox_state, "pending");
    assert_eq!(record.outbox_action, "recovery_dispatch");
    assert_eq!(
        record.outbox_destination.as_deref(),
        Some("distributed_advance")
    );
    assert_eq!(record.attempt, 0);
    assert!(record.claim_token.is_none());
    assert!(record.lease_expires_at_ms.is_none());
    assert!(record.delivered_at_ms.is_none());
    assert_eq!(
        ControllerCommandWakeRecord::from_value(&record.to_value()).expect("wake record roundtrip"),
        *record
    );

    let claimed = store
        .claim_controller_command_wake(
            &command.command_id,
            &command.command_digest,
            "wake-owner",
            10,
            1,
        )
        .expect("claim wake")
        .expect("claimed wake");
    assert_eq!(claimed.outbox_state, "claimed");
    let expired = store
        .reap_controller_command_wakes(checkpoint_key, 10)
        .expect("expired reaper");
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].outbox_state, "pending");
    assert_eq!(expired[0].attempt, 1);
    assert!(expired[0].claim_token.is_none());
    assert!(expired[0].lease_expires_at_ms.is_none());

    let claimed_again = store
        .claim_controller_command_wake(
            &command.command_id,
            &command.command_digest,
            "wake-owner-2",
            20,
            11,
        )
        .expect("reclaim wake")
        .expect("reclaimed wake");
    store
        .complete_controller_command_wake(
            &command.command_id,
            &command.command_digest,
            "wake-owner-2",
            claimed_again.outbox_attempt,
            "ambiguous",
            12,
            Some("provider outcome unknown"),
        )
        .expect("mark ambiguous")
        .expect("ambiguous wake");
    assert!(store
        .reap_controller_command_wakes(checkpoint_key, 100)
        .expect("ambiguous reaper")
        .is_empty());
}

#[test]
fn controller_wake_reaper_returns_full_record_memory_and_sqlite() {
    let memory = InMemoryCheckpointStore::new();
    assert_full_reaper_record(&memory, "wake-record-memory", "wake-record-memory-command");

    let directory = tempdir().expect("tempdir");
    let sqlite =
        SqliteCheckpointStore::new(directory.path().join("wake-record.sqlite")).expect("sqlite");
    assert_full_reaper_record(&sqlite, "wake-record-sqlite", "wake-record-sqlite-command");
}
