use super::*;

#[test]
fn notification_reconciliation_uses_abort_wire_value() {
    let store = InMemoryCheckpointStore::new();
    let mut checkpoint = minimal_checkpoint();
    checkpoint.checkpoint_key = "checkpoint-notification-abort".to_string();
    let key = checkpoint.checkpoint_key.clone();
    store.create_checkpoint(checkpoint).expect("create");
    let claimed = store
        .claim_checkpoint(
            &key,
            1,
            "notification-worker",
            1_000_000,
            0,
            ClaimMode::Continue,
        )
        .expect("claim")
        .expect("claimed");
    let request = HostInteractionRequest::new(
        "notification-abort-interaction",
        1,
        "notification-abort-operation",
        "notification-abort-tool",
        "Choose.",
    )
    .expect("request");
    let context = HostInteractionAdmissionContext::new(
        &key,
        claimed.revision,
        "notification-worker",
        1,
        0,
        claimed.lease_expires_at_ms.expect("lease"),
    )
    .expect("context");
    let admitted = store
        .produce_host_interaction(request, &context)
        .expect("produce");
    let claimed_notification = store
        .claim_host_interaction_notification(
            &admitted.notification_id,
            &admitted.notification_payload_digest,
            "notification-delivery",
            1_000_000,
            0,
        )
        .expect("claim notification")
        .expect("notification row");
    let ambiguous = store
        .complete_host_interaction_notification(
            &admitted.notification_id,
            &admitted.notification_payload_digest,
            claimed_notification
                .claim_token
                .as_deref()
                .expect("notification claim token"),
            claimed_notification.attempt,
            "ambiguous",
            1,
            Some("observer crashed after delivery"),
        )
        .expect("mark notification ambiguous")
        .expect("ambiguous row");
    assert_eq!(
        ambiguous.outbox_state,
        vv_agent::NotificationOutboxState::Ambiguous
    );
    let aborted = store
        .reconcile_host_interaction_notification(
            &admitted.notification_id,
            &admitted.notification_payload_digest,
            "abort",
            2,
            Some("observer unavailable"),
        )
        .expect("reconcile abort")
        .expect("aborted row");
    assert_eq!(
        aborted.outbox_state,
        vv_agent::NotificationOutboxState::Aborted
    );
    assert_eq!(
        aborted.abort_reason.as_deref(),
        Some("observer unavailable")
    );
    assert_eq!(
        store
            .reconcile_host_interaction_notification(
                &admitted.notification_id,
                &admitted.notification_payload_digest,
                "abort",
                3,
                Some("observer unavailable"),
            )
            .expect("same abort notification replay")
            .expect("replayed aborted row")
            .outbox_state,
        vv_agent::NotificationOutboxState::Aborted
    );
    let stale = store
        .reconcile_host_interaction_notification(
            &admitted.notification_id,
            &admitted.notification_payload_digest,
            "retry",
            4,
            None,
        )
        .expect_err("closed notification cannot move back to pending");
    assert_eq!(stale.code(), "notification_stale");
    assert_eq!(
        store
            .reconcile_host_interaction_notification(
                &admitted.notification_id,
                &"0".repeat(64),
                "abort",
                5,
                Some("observer unavailable"),
            )
            .expect_err("different notification digest must conflict")
            .code(),
        "notification_conflict"
    );
}
