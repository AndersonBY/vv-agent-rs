use serde_json::Value;
use sha2::{Digest, Sha256};
use vv_agent::RunEvent;

const FIXTURE: &str = include_str!("fixtures/parity/run_events_v1_invalid.json");
const FIXTURE_SHA256: &str = "deec9e8c56cdb39e70b8c40e776021ce669dc6ea3477bd9b23f947dd5b5f1e99";

fn contract() -> Value {
    assert_eq!(
        format!("{:x}", Sha256::digest(FIXTURE.as_bytes())),
        FIXTURE_SHA256
    );
    serde_json::from_str(FIXTURE).expect("run event invalid fixture")
}

#[test]
fn run_event_v1_compatibility_inputs_canonicalize_to_fixture() {
    let contract = contract();
    for case in contract["canonicalize"]
        .as_array()
        .expect("canonical cases")
    {
        let event: RunEvent = serde_json::from_value(case["input"].clone())
            .unwrap_or_else(|error| panic!("{}: {error}", case["id"]));
        let encoded = serde_json::to_value(event).expect("serialize canonical event");
        assert_eq!(encoded, case["output"], "{}", case["id"]);
    }
}

#[test]
fn run_event_v1_invalid_inputs_are_rejected() {
    let contract = contract();
    for case in contract["reject"].as_array().expect("reject cases") {
        let result = serde_json::from_value::<RunEvent>(case["input"].clone());
        assert!(result.is_err(), "{}", case["id"]);
    }
}
