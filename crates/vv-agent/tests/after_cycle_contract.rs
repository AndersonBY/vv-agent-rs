use std::collections::BTreeMap;

use serde_json::{json, Value};
use vv_agent::runtime::lifecycle::{
    persist_after_cycle_disallowed_tools, read_after_cycle_disallowed_tools,
    AFTER_CYCLE_CONTROL_SCHEMA, AFTER_CYCLE_CONTROL_STATE_KEY, MAX_DISALLOW_TOOLS,
    MAX_STEERING_MESSAGES, MAX_STEERING_MESSAGE_UTF8_BYTES, MAX_STOP_CODE_ASCII_BYTES,
    MAX_STOP_MESSAGE_UTF8_BYTES, MAX_TOOL_NAME_UTF8_BYTES, MAX_TOTAL_STEERING_UTF8_BYTES,
};
use vv_agent::{AfterCycleAction, AfterCycleDecision};

const FIXTURE: &str = include_str!("fixtures/parity/after_cycle_hook_v1.json");

#[test]
fn after_cycle_limits_and_closed_actions_match_canonical_fixture() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("fixture");
    let limits = &fixture["decision"]["limits"];
    assert_eq!(limits["max_steering_messages"], MAX_STEERING_MESSAGES);
    assert_eq!(
        limits["max_steering_message_utf8_bytes"],
        MAX_STEERING_MESSAGE_UTF8_BYTES
    );
    assert_eq!(
        limits["max_total_steering_utf8_bytes"],
        MAX_TOTAL_STEERING_UTF8_BYTES
    );
    assert_eq!(limits["max_disallow_tools"], MAX_DISALLOW_TOOLS);
    assert_eq!(limits["max_tool_name_utf8_bytes"], MAX_TOOL_NAME_UTF8_BYTES);
    assert_eq!(
        limits["max_stop_code_ascii_bytes"],
        MAX_STOP_CODE_ASCII_BYTES
    );
    assert_eq!(
        limits["max_stop_message_utf8_bytes"],
        MAX_STOP_MESSAGE_UTF8_BYTES
    );
    assert_eq!(
        fixture["decision"]["action_values"],
        json!(["continue", "steer", "stop_non_success"])
    );
    assert_eq!(
        serde_json::to_value(AfterCycleAction::StopNonSuccess).unwrap(),
        json!("stop_non_success")
    );
}

#[test]
fn after_cycle_decision_deserialization_rejects_unknown_expansion_fields() {
    let error = serde_json::from_value::<AfterCycleDecision>(json!({
        "action": "continue",
        "steering_messages": [],
        "disallow_tools": [],
        "allow_tools": ["bash"],
        "stop": null,
    }))
    .unwrap_err();

    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn after_cycle_control_state_uses_utf16_order_and_is_strictly_validated() {
    let mut shared_state = BTreeMap::new();
    let supplementary = "\u{10000}".to_string();
    let private_use = "\u{e000}".to_string();
    let persisted = persist_after_cycle_disallowed_tools(
        &mut shared_state,
        &[private_use.clone(), supplementary.clone()],
    )
    .expect("persisted state");

    assert_eq!(persisted, [supplementary.clone(), private_use.clone()]);
    assert_eq!(
        shared_state[AFTER_CYCLE_CONTROL_STATE_KEY],
        json!({
            "schema_version": AFTER_CYCLE_CONTROL_SCHEMA,
            "disallowed_tools": [supplementary, private_use],
        })
    );
    assert_eq!(
        read_after_cycle_disallowed_tools(&shared_state).expect("valid state"),
        persisted
    );

    shared_state.insert(
        AFTER_CYCLE_CONTROL_STATE_KEY.to_string(),
        json!({
            "schema_version": AFTER_CYCLE_CONTROL_SCHEMA,
            "disallowed_tools": ["bash"],
            "allow_tools": ["bash"],
        }),
    );
    let error = read_after_cycle_disallowed_tools(&shared_state).unwrap_err();
    assert_eq!(error.code, "after_cycle_control_state_invalid");
}
