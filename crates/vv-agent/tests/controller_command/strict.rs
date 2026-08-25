use super::*;

#[test]
fn strict_v8_readers_reject_old_versions_and_unknown_members() {
    let request =
        HostInteractionRequest::new("interaction-strict", 1, "operation", "tool", "prompt")
            .expect("request");
    let mut request_wire = request.to_value();
    request_wire["unexpected"] = json!(true);
    assert_eq!(
        HostInteractionRequest::from_value(&request_wire)
            .expect_err("unknown request field")
            .code(),
        "host_interaction_fields_invalid"
    );
    let mut old_request = request.to_value();
    old_request["schema_version"] = json!("vv-agent.host-interaction-request.v0");
    assert_eq!(
        HostInteractionRequest::from_value(&old_request)
            .expect_err("old request version")
            .code(),
        "host_interaction_fields_invalid"
    );
    let command = ControllerCommand::new(
        "command-strict",
        ControllerHandle::new("checkpoint", "run", "trace").expect("handle"),
        1,
        0,
        ControllerCommandVariant::Suspend,
    )
    .expect("command");
    let mut command_wire = command.to_value();
    command_wire["legacy_alias"] = json!("reject");
    assert_eq!(
        ControllerCommand::from_value(&command_wire)
            .expect_err("unknown command field")
            .code(),
        "controller_command_digest_invalid"
    );
    command_wire
        .as_object_mut()
        .expect("command object")
        .remove("legacy_alias");
    command_wire["schema_version"] = json!("vv-agent.controller-command.v0");
    assert_eq!(
        ControllerCommand::from_value(&command_wire)
            .expect_err("old command version")
            .code(),
        "controller_command_digest_invalid"
    );
}

#[test]
fn host_request_and_public_outbox_redact_credentials_and_external_locators() {
    let original = "Authorization: Bearer sk-live-123 secret=abc Bearer bare-secret https://example.test/callback?token=xyz";
    let request = HostInteractionRequest::new(
        "interaction-redaction",
        1,
        "operation-redaction",
        "tool-redaction",
        original,
    )
    .expect("request");
    let text = &request.prompt;
    assert!(!text.contains("sk-live-123"));
    assert!(!text.contains("abc"));
    assert!(!text.contains("bare-secret"));
    assert!(!text.contains("https://example.test"));
    assert!(!text.contains("token=xyz"));
    let response = HostInteractionMessage::user(original).expect("response");
    assert!(!response.content.contains("sk-live-123"));
    assert!(!response.content.contains("bare-secret"));
    assert!(!response.content.contains("https://example.test"));

    // A wire producer may have serialized the unsanitized text while using
    // the canonical (sanitized) digest.  The strict reader must normalize at
    // the CAS boundary and retain only the redacted response.
    let command = ControllerCommand::new(
        "command-response-redaction",
        ControllerHandle::new("checkpoint-redaction", "run-redaction", "trace-redaction")
            .expect("handle"),
        1,
        0,
        ControllerCommandVariant::HostInteractionResponse {
            interaction_id: "interaction-redaction".to_string(),
            logical_cycle: 1,
            operation_id: "operation-redaction".to_string(),
            tool_call_id: "tool-redaction".to_string(),
            request_digest: request.request_digest.clone(),
            response,
        },
    )
    .expect("command");
    let mut command_wire = command.to_value();
    command_wire["command"]["response"]["content"] = json!(original);
    let parsed = ControllerCommand::from_value(&command_wire).expect("sanitized command wire");
    if let ControllerCommandVariant::HostInteractionResponse { response, .. } = parsed.command {
        assert!(!response.content.contains("sk-live-123"));
        assert!(!response.content.contains("bare-secret"));
        assert!(!response.content.contains("https://example.test"));
    } else {
        panic!("expected host interaction response command");
    }

    let store = InMemoryCheckpointStore::new();
    let checkpoint = minimal_checkpoint();
    let key = checkpoint.checkpoint_key.clone();
    store.create_checkpoint(checkpoint).expect("create");
    store
        .claim_checkpoint(
            &key,
            1,
            "redaction-worker",
            1_000_000,
            0,
            ClaimMode::Continue,
        )
        .expect("claim");
    let admitted = store
        .produce_host_interaction(
            request.clone(),
            &admission_context(&store, &key, "redaction-worker", 0),
        )
        .expect("produce");
    let persisted = store
        .load_checkpoint(&key)
        .expect("load")
        .expect("checkpoint");
    let checkpoint_prompt = persisted
        .active_host_interaction
        .expect("active request")
        .prompt;
    let notification = store
        .get_host_interaction_notification(&admitted.notification_id)
        .expect("notification")
        .expect("notification row");
    for text in [&checkpoint_prompt, &notification.payload.prompt] {
        assert!(!text.contains("sk-live-123"));
        assert!(!text.contains("https://example.test"));
        assert!(!text.contains("token=xyz"));
    }
}

#[test]
fn host_response_decoder_rejects_unsanitized_content_before_digest_use() {
    let request = HostInteractionRequest::new(
        "interaction-response-wire",
        1,
        "operation-response-wire",
        "tool-response-wire",
        "Choose.",
    )
    .expect("request");
    let response = HostInteractionResponse::new(
        request.interaction_id.clone(),
        request.logical_cycle,
        request.operation_id.clone(),
        request.tool_call_id.clone(),
        request.request_digest.clone(),
        "command-response-wire",
        HostInteractionMessage::user("safe response").expect("response"),
    )
    .expect("response record");
    let mut wire = response.to_value();
    wire["response"]["content"] = json!("Authorization: Bearer sk-secret");
    assert_eq!(
        HostInteractionResponse::from_value(&wire)
            .expect_err("decoder must reject raw response content")
            .code(),
        "host_interaction_response_missing"
    );

    let invalid_role = HostInteractionMessage {
        role: "assistant".to_string(),
        content: "safe response".to_string(),
    };
    assert_eq!(
        HostInteractionResponse::new(
            request.interaction_id,
            request.logical_cycle,
            request.operation_id,
            request.tool_call_id,
            request.request_digest,
            "command-invalid-role",
            invalid_role,
        )
        .expect_err("constructor must reject a non-user response role")
        .code(),
        "host_interaction_response_missing"
    );
}

#[test]
fn suspended_host_response_waits_for_explicit_resume_before_recovery_wake() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint = minimal_checkpoint();
    let key = checkpoint.checkpoint_key.clone();
    let run_id = checkpoint.root_run_id.clone();
    let trace_id = checkpoint.trace_id.clone();
    store.create_checkpoint(checkpoint).expect("create");
    store
        .claim_checkpoint(&key, 1, "worker-suspend", 1_000_000, 0, ClaimMode::Continue)
        .expect("claim");
    let request = HostInteractionRequest::new(
        "interaction-suspended",
        1,
        "operation-suspended",
        "tool-suspended",
        "Choose.",
    )
    .expect("request");
    let admitted = store
        .produce_host_interaction(
            request.clone(),
            &admission_context(&store, &key, "worker-suspend", 0),
        )
        .expect("produce");
    let handle = ControllerHandle::new(&key, &run_id, &trace_id).expect("handle");
    let suspend = ControllerCommand::new(
        "command-suspend-host",
        handle.clone(),
        1,
        admitted.checkpoint_revision,
        ControllerCommandVariant::Suspend,
    )
    .expect("suspend");
    let suspended = store
        .resolve_controller_command(suspend)
        .expect("suspend host interaction");
    let suspended_revision = match suspended {
        ControllerCommandResolution::Applied { receipt, .. } => receipt.resulting_revision,
        other => panic!("unexpected resolution: {other:?}"),
    };
    let response = ControllerCommand::new(
        "command-response-suspended",
        handle.clone(),
        1,
        suspended_revision,
        ControllerCommandVariant::HostInteractionResponse {
            interaction_id: request.interaction_id.clone(),
            logical_cycle: 1,
            operation_id: request.operation_id.clone(),
            tool_call_id: request.tool_call_id.clone(),
            request_digest: request.request_digest.clone(),
            response: HostInteractionMessage::user("later").expect("response"),
        },
    )
    .expect("response");
    let response_resolution = store
        .resolve_controller_command(response)
        .expect("response while suspended");
    let response_revision = match response_resolution {
        ControllerCommandResolution::Applied { receipt, wake } => {
            assert_eq!(wake.action, "none");
            assert_eq!(receipt.resulting_status, "suspended");
            receipt.resulting_revision
        }
        other => panic!("unexpected resolution: {other:?}"),
    };
    let resume = ControllerCommand::new(
        "command-resume-host",
        handle,
        1,
        response_revision,
        ControllerCommandVariant::Resume,
    )
    .expect("resume");
    match store
        .resolve_controller_command(resume)
        .expect("resume host interaction")
    {
        ControllerCommandResolution::Applied { receipt, wake } => {
            assert_eq!(receipt.resulting_status, "running");
            assert_eq!(wake.action, "recovery_dispatch");
            assert_eq!(wake.logical_cycle, 1);
        }
        other => panic!("unexpected resolution: {other:?}"),
    }
}

#[test]
fn concurrent_recovery_calls_linearize_to_one_injection() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint = minimal_checkpoint();
    let key = checkpoint.checkpoint_key.clone();
    let run_id = checkpoint.root_run_id.clone();
    let trace_id = checkpoint.trace_id.clone();
    store.create_checkpoint(checkpoint).expect("create");
    store
        .claim_checkpoint(
            &key,
            1,
            "worker-concurrent",
            1_000_000,
            0,
            ClaimMode::Continue,
        )
        .expect("claim");
    let request = HostInteractionRequest::new(
        "interaction-concurrent",
        1,
        "operation-concurrent",
        "tool-concurrent",
        "Choose.",
    )
    .expect("request");
    let admitted = store
        .produce_host_interaction(
            request.clone(),
            &admission_context(&store, &key, "worker-concurrent", 0),
        )
        .expect("produce");
    let command_id = "command-concurrent";
    let command = ControllerCommand::new(
        command_id,
        ControllerHandle::new(&key, &run_id, &trace_id).expect("handle"),
        1,
        admitted.checkpoint_revision,
        ControllerCommandVariant::HostInteractionResponse {
            interaction_id: request.interaction_id.clone(),
            logical_cycle: 1,
            operation_id: request.operation_id.clone(),
            tool_call_id: request.tool_call_id.clone(),
            request_digest: request.request_digest.clone(),
            response: HostInteractionMessage::user("concurrent").expect("response"),
        },
    )
    .expect("command");
    store
        .resolve_controller_command(command)
        .expect("admit response");
    let envelope = HostInteractionRecoveryEnvelope {
        schema_version: "vv-agent.host-interaction-recovery.v1".to_string(),
        record_id: admitted.record_id,
        checkpoint_key: key.clone(),
        run_id,
        trace_id,
        claim_mode: "recovery".to_string(),
        resume_attempt: 1,
        expected_revision: admitted.checkpoint_revision + 1,
        logical_cycle: 1,
        interaction_id: request.interaction_id,
        operation_id: request.operation_id,
        tool_call_id: request.tool_call_id,
        request_digest: request.request_digest,
        command_id: command_id.to_string(),
    };
    let first_store = store.clone();
    let second_store = store.clone();
    let first_envelope = envelope.clone();
    let second_envelope = envelope;
    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(move || {
            first_store
                .claim_and_consume_host_interaction_response(first_envelope)
                .expect("first recovery")
        });
        let second = scope.spawn(move || {
            second_store
                .claim_and_consume_host_interaction_response(second_envelope)
                .expect("second recovery")
        });
        (
            first.join().expect("first join"),
            second.join().expect("second join"),
        )
    });
    assert!(
        (first.kind == "applied" && second.kind == "replayed")
            || (first.kind == "replayed" && second.kind == "applied")
    );
    assert_eq!(
        store
            .load_checkpoint(&key)
            .expect("load")
            .expect("checkpoint")
            .messages
            .iter()
            .filter(|message| message.content == "concurrent")
            .count(),
        1
    );
}

#[test]
fn host_record_state_boundaries_reject_partial_terminal_fields() {
    let request =
        HostInteractionRequest::new("interaction-record", 1, "operation", "tool", "prompt")
            .expect("request");
    let mut record = HostInteractionRecord {
        schema_version: "vv-agent.host-interaction-record.v1".to_string(),
        record_id: {
            let identity = json!({
                "schema_version": "vv-agent.host-interaction-record.v1",
                "checkpoint_key": "checkpoint-id",
                "interaction_id": request.interaction_id,
                "logical_cycle": request.logical_cycle,
                "request_digest": request.request_digest,
            });
            format!(
                "{:x}",
                Sha256::digest(
                    vv_agent::canonical_json_bytes(&identity, "record identity")
                        .expect("canonical identity")
                )
            )
        },
        checkpoint_key: "checkpoint-id".to_string(),
        interaction_id: request.interaction_id.clone(),
        logical_cycle: request.logical_cycle,
        attempt: 0,
        claim_token: None,
        lease_expires_at_ms: None,
        request: request.clone(),
        request_digest: request.request_digest.clone(),
        state: "active".to_string(),
        response: None,
        response_digest: None,
        command_id: None,
        resolved_revision: None,
        consumed_revision: None,
        last_error: None,
    };
    record.validate().expect("active record");
    record.resolved_revision = Some(1);
    assert_eq!(
        record
            .validate()
            .expect_err("active resolved revision")
            .code(),
        "host_interaction_fields_invalid"
    );
    record.state = "resolved_pending".to_string();
    record.resolved_revision = None;
    assert_eq!(
        record
            .validate()
            .expect_err("pending record without revision")
            .code(),
        "host_interaction_fields_invalid"
    );
    let mut wire = record.to_value();
    wire["unknown"] = json!(true);
    assert_eq!(
        HostInteractionRecord::from_value(&wire)
            .expect_err("record unknown field")
            .code(),
        "host_interaction_fields_invalid"
    );
}

#[test]
fn cross_run_controller_handle_is_rejected_without_revision_change() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint = minimal_checkpoint();
    let key = checkpoint.checkpoint_key.clone();
    let trace_id = checkpoint.trace_id.clone();
    store.create_checkpoint(checkpoint).expect("create");
    let command = ControllerCommand::new(
        "command-cross-run",
        ControllerHandle::new(&key, "wrong-run", &trace_id).expect("handle"),
        1,
        0,
        ControllerCommandVariant::Suspend,
    )
    .expect("command");
    assert!(matches!(
        store
            .resolve_controller_command(command)
            .expect("cross-run command is a closed rejected resolution"),
        ControllerCommandResolution::Rejected { error }
            if error.starts_with("controller_command_stale:")
    ));
    assert_eq!(
        store
            .load_checkpoint(&key)
            .expect("load")
            .expect("checkpoint")
            .revision,
        0
    );
}

#[test]
fn response_identity_conflict_cannot_mutate_pending_interaction() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint = minimal_checkpoint();
    let key = checkpoint.checkpoint_key.clone();
    let run_id = checkpoint.root_run_id.clone();
    let trace_id = checkpoint.trace_id.clone();
    store.create_checkpoint(checkpoint).expect("create");
    store
        .claim_checkpoint(
            &key,
            1,
            "worker-response-conflict",
            1_000_000,
            0,
            ClaimMode::Continue,
        )
        .expect("claim");
    let request =
        HostInteractionRequest::new("interaction-conflict", 1, "operation", "tool", "prompt")
            .expect("request");
    let admitted = store
        .produce_host_interaction(
            request.clone(),
            &admission_context(&store, &key, "worker-response-conflict", 0),
        )
        .expect("produce");
    let command = ControllerCommand::new(
        "command-response-conflict",
        ControllerHandle::new(&key, &run_id, &trace_id).expect("handle"),
        1,
        admitted.checkpoint_revision,
        ControllerCommandVariant::HostInteractionResponse {
            interaction_id: request.interaction_id,
            logical_cycle: 1,
            operation_id: "wrong-operation".to_string(),
            tool_call_id: request.tool_call_id,
            request_digest: request.request_digest,
            response: HostInteractionMessage::user("wrong").expect("response"),
        },
    )
    .expect("command");
    assert!(matches!(
        store
            .resolve_controller_command(command)
            .expect("identity conflict is a closed rejected resolution"),
        ControllerCommandResolution::Rejected { error }
            if error.starts_with("controller_command_stale:")
    ));
    assert_eq!(
        store
            .load_checkpoint(&key)
            .expect("load")
            .expect("checkpoint")
            .revision,
        admitted.checkpoint_revision
    );
}

#[test]
fn receipt_wire_rejects_unknown_fields_after_store_admission() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint = minimal_checkpoint();
    let key = checkpoint.checkpoint_key.clone();
    let handle =
        ControllerHandle::new(&key, &checkpoint.root_run_id, &checkpoint.trace_id).expect("handle");
    store.create_checkpoint(checkpoint).expect("create");
    let command = ControllerCommand::new(
        "command-receipt-wire",
        handle,
        1,
        0,
        ControllerCommandVariant::Suspend,
    )
    .expect("command");
    let receipt = match store.resolve_controller_command(command).expect("resolve") {
        ControllerCommandResolution::Applied { receipt, .. } => receipt,
        other => panic!("unexpected resolution: {other:?}"),
    };
    let mut wire = receipt.to_value();
    wire["unknown"] = json!(true);
    assert_eq!(
        vv_agent::ControllerCommandReceipt::from_value(&wire)
            .expect_err("receipt unknown field")
            .code(),
        "controller_command_invalid_state"
    );
}
