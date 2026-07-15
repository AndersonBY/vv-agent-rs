use serde_json::{json, Value};
use vv_agent::RunEvent;

fn run_completed_event() -> Value {
    json!({
        "version": "v1",
        "type": "run_completed",
        "event_id": "evt_completion_contract",
        "run_id": "run_completion_contract",
        "trace_id": "trace_completion_contract",
        "created_at": 1.0,
        "status": "completed",
        "final_output": "done"
    })
}

fn with_field(field: &str, value: Value) -> Value {
    let mut event = run_completed_event();
    event
        .as_object_mut()
        .expect("run event object")
        .insert(field.to_string(), value);
    event
}

#[test]
fn completion_fields_accept_null_and_declared_strings() {
    for reason in [
        "tool_finish",
        "no_tool_finish",
        "stop_on_first_tool",
        "stop_at_tool_name",
        "wait_user",
        "max_cycles",
        "cancelled",
        "failed",
        "budget_exhausted",
    ] {
        let event: RunEvent =
            serde_json::from_value(with_field("completion_reason", json!(reason)))
                .unwrap_or_else(|error| panic!("{reason}: {error}"));
        assert_eq!(
            event.completion_reason().map(|value| value.as_str()),
            Some(reason)
        );
    }

    let mut nullable = run_completed_event();
    let object = nullable.as_object_mut().expect("run event object");
    object.insert("completion_reason".to_string(), Value::Null);
    object.insert("completion_tool_name".to_string(), Value::Null);
    object.insert("partial_output".to_string(), Value::Null);
    let nullable: RunEvent = serde_json::from_value(nullable).expect("nullable completion fields");
    assert_eq!(nullable.completion_reason(), None);
    assert_eq!(nullable.completion_tool_name(), None);
    assert_eq!(nullable.partial_output(), None);

    let mut strings = run_completed_event();
    let object = strings.as_object_mut().expect("run event object");
    object.insert("completion_tool_name".to_string(), json!("task_finish"));
    object.insert("partial_output".to_string(), json!("last draft"));
    let strings: RunEvent = serde_json::from_value(strings).expect("string completion fields");
    assert_eq!(strings.completion_tool_name(), Some("task_finish"));
    assert_eq!(strings.partial_output(), Some("last draft"));
}

#[test]
fn completion_fields_reject_unknown_reason_and_wrong_types() {
    for (field, value) in [
        ("completion_reason", json!("future_reason")),
        ("completion_reason", json!(7)),
        ("completion_tool_name", json!(false)),
        ("completion_tool_name", json!(["task_finish"])),
        ("partial_output", json!({"text": "last draft"})),
        ("partial_output", json!(7)),
    ] {
        let error = serde_json::from_value::<RunEvent>(with_field(field, value))
            .expect_err("invalid completion field must be rejected")
            .to_string();
        assert!(error.contains(field), "{field}: {error}");
    }
}
