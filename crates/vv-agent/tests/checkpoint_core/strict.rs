use super::*;

#[test]
fn succeeded_tool_journal_recomputes_result_digest() {
    let mut entry = journal_case("tool_succeeded").to_value();
    entry["result_digest"] = json!("0".repeat(64));
    let error = OperationJournalEntry::from_value(&entry).expect_err("digest mismatch");
    assert_eq!(error.code(), "tool_receipt_digest_invalid");
}

#[test]
fn operation_error_journal_reader_round_trips_and_rejects_tampering() {
    let canonical = journal_case("tool_failed").to_value();
    let decoded = OperationJournalEntry::from_value(&canonical).expect("canonical journal entry");
    assert_eq!(decoded.to_value(), canonical);

    let error = canonical
        .get("error")
        .cloned()
        .expect("failed journal error");
    let decoded_error = OperationError::from_value(&error).expect("canonical operation error");
    assert_eq!(decoded_error.to_value(), error);
    assert_eq!(
        serde_json::from_value::<OperationError>(error.clone()).expect("serde operation error"),
        decoded_error
    );

    let mut unknown = canonical.clone();
    unknown["error"]["future"] = json!(true);
    assert!(OperationJournalEntry::from_value(&unknown).is_err());

    for field in ["code", "message", "retryable"] {
        let mut missing = canonical.clone();
        missing["error"]
            .as_object_mut()
            .expect("error object")
            .remove(field);
        assert!(
            OperationJournalEntry::from_value(&missing).is_err(),
            "missing operation error field {field}"
        );

        let mut null = canonical.clone();
        null["error"][field] = Value::Null;
        assert!(
            OperationJournalEntry::from_value(&null).is_err(),
            "null operation error field {field}"
        );
    }

    let mut serde_unknown = error.clone();
    serde_unknown["future"] = json!(true);
    assert!(serde_json::from_value::<OperationError>(serde_unknown).is_err());

    for field in ["code", "message", "retryable"] {
        let mut missing = error.clone();
        missing.as_object_mut().expect("error object").remove(field);
        assert!(serde_json::from_value::<OperationError>(missing).is_err());

        let mut null = error.clone();
        null[field] = Value::Null;
        assert!(serde_json::from_value::<OperationError>(null).is_err());
    }
}

#[test]
fn checkpoint_cycle_reader_rejects_noncanonical_nested_shapes() {
    let valid = json!({
        "index": 1,
        "assistant_message": "done",
        "tool_calls": [],
        "tool_results": [],
        "memory_compacted": false
    });
    CycleRecord::from_dict(&valid).expect("canonical cycle");

    let mut unknown = valid.clone();
    unknown["future"] = json!(true);
    assert!(CycleRecord::from_dict(&unknown).is_err());

    let mut missing = valid.clone();
    missing
        .as_object_mut()
        .expect("cycle object")
        .remove("memory_compacted");
    assert!(CycleRecord::from_dict(&missing).is_err());

    let mut nested_call = valid.clone();
    nested_call["tool_calls"] = json!([{
        "id": "call-1",
        "name": "tool",
        "arguments": {},
        "future": true
    }]);
    assert!(CycleRecord::from_dict(&nested_call).is_err());

    let mut nested_result = valid;
    nested_result["tool_results"] = json!([{
        "tool_call_id": "call-1",
        "content": "ok",
        "status_code": "SUCCESS",
        "directive": "continue",
        "future": true
    }]);
    assert!(CycleRecord::from_dict(&nested_result).is_err());
}

#[test]
fn event_cursor_readers_reject_tampered_shapes() {
    let valid = current_codec_case("claimed_active_cycle");
    checkpoint_from_value(&valid, 262_144).expect("canonical event cursor");

    let mut unknown = valid.clone();
    unknown["event_cursor"]["future"] = json!(true);
    assert!(checkpoint_from_value(&unknown, 262_144).is_err());

    let mut missing = valid.clone();
    missing["event_cursor"]
        .as_object_mut()
        .expect("event cursor object")
        .remove("value");
    assert!(checkpoint_from_value(&missing, 262_144).is_err());

    let mut wrong_type = valid.clone();
    wrong_type["event_cursor"]["last_event_id"] = json!(true);
    assert!(checkpoint_from_value(&wrong_type, 262_144).is_err());

    let mut nested_unknown = valid.clone();
    nested_unknown["event_cursor"]["store_ref"]["future"] = json!(true);
    assert!(checkpoint_from_value(&nested_unknown, 262_144).is_err());

    let mut nested_wrong_type = valid;
    nested_wrong_type["event_cursor"]["store_ref"]["version"] = json!(2);
    assert!(checkpoint_from_value(&nested_wrong_type, 262_144).is_err());
}

#[test]
fn event_outbox_cursor_uses_the_same_strict_codec() {
    let event_id = "evt-cursor-codec";
    let mut entry = EventOutboxEntry::pending(event_id, current_event(event_id)).unwrap();
    entry.state = "delivered".to_string();
    entry.cursor = Some(serde_json::to_value(delivery_cursor(event_id, 1)).unwrap());
    entry.validate().expect("canonical delivered cursor");

    let mut unknown = entry.to_value();
    unknown["cursor"]["future"] = json!(true);
    assert!(EventOutboxEntry::from_value(&unknown).is_err());

    let mut nested_unknown = entry.to_value();
    nested_unknown["cursor"]["store_ref"]["future"] = json!(true);
    assert!(EventOutboxEntry::from_value(&nested_unknown).is_err());

    let mut wrong_type = entry.to_value();
    wrong_type["cursor"]["last_event_id"] = json!(false);
    assert!(EventOutboxEntry::from_value(&wrong_type).is_err());
}
