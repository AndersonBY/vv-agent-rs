use serde_json::Value;
use sha2::{Digest, Sha256};
use vv_agent::RunEvent;

const FIXTURE: &str = include_str!("fixtures/parity/run_events_v1_invalid.json");
const FIXTURE_SHA256: &str = "55e3be856d8c1cc1c522cefa8bb0d0aa05b4552e7eda34c0a6c5c04172394e06";

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
