use serde_json::{json, Value};
use vv_agent::{AgentResult, EndpointConfig, EndpointOption, ResolvedModelConfig, RunResult};

fn contract() -> Value {
    serde_json::from_str(include_str!("fixtures/parity/result_public.json"))
        .expect("result public fixture")
}

fn result() -> RunResult {
    let contract = contract();
    let raw_result = AgentResult::from_dict(&contract["agent_result"]).expect("agent result");
    let resolved = ResolvedModelConfig::new(
        "test",
        "requested",
        "selected",
        "model-id",
        vec![EndpointOption::new(
            EndpointConfig::new(
                "endpoint-public",
                "secret-must-not-serialize",
                "https://example.invalid/v1",
            ),
            "model-id",
        )],
    );
    RunResult::new("assistant", raw_result, resolved)
        .with_input("approve it")
        .with_metadata(std::collections::BTreeMap::from([(
            "tenant".to_string(),
            json!("acme"),
        )]))
}

#[test]
fn approval_snapshot_and_state_match_shared_contract() {
    let contract = contract();
    let result = result();

    assert_eq!(
        serde_json::to_value(result.approvals()).expect("approval projection"),
        contract["expected_approvals"]
    );
    let mut state = result.into_state().expect("wait-user state");
    state.approve("approval_1").expect("approve");
    let mut expected = contract["expected_approvals"].clone();
    expected[0]["approved"] = json!(true);
    assert_eq!(
        serde_json::to_value(state.approvals()).expect("approved projection"),
        expected
    );
    assert_eq!(state.pending_approval_ids(), vec!["approval_1"]);
    assert_eq!(state.approved_interruption_ids(), &["approval_1"]);
}

#[test]
fn run_result_public_projection_matches_shared_contract_without_credentials() {
    let contract = contract();
    let result = result();
    let projection = result.to_value();
    let mut keys = projection
        .as_object()
        .expect("projection object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();

    assert_eq!(json!(keys), contract["projection_keys"]);
    assert_eq!(projection["status"], "wait_user");
    assert_eq!(projection["final_output"], "Approval is required.");
    assert_eq!(
        projection["token_usage"],
        contract["agent_result"]["token_usage"]
    );
    assert_eq!(
        projection["resolved_model"],
        contract["resolved_model_projection"]
    );
    assert!(!serde_json::to_string(&result)
        .expect("serialize result")
        .contains("secret-must-not-serialize"));
    assert_eq!(result.to_dict(), projection);
}

#[test]
fn agent_result_reader_enforces_the_closed_current_wire() {
    let contract = contract();
    let raw = &contract["agent_result"];
    let wire = &contract["agent_result_wire"];

    assert_eq!(AgentResult::from_dict(raw).unwrap().to_dict(), *raw);
    for field in wire["required_fields"].as_array().unwrap() {
        let mut invalid = raw.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .remove(field.as_str().unwrap());
        assert!(AgentResult::from_dict(&invalid).is_err(), "{field}");
    }
    for field in wire["optional_fields"].as_array().unwrap() {
        let mut invalid = raw.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .insert(field.as_str().unwrap().to_string(), Value::Null);
        assert!(AgentResult::from_dict(&invalid).is_err(), "{field}");
    }
    let mut unknown = raw.clone();
    unknown
        .as_object_mut()
        .unwrap()
        .insert("legacy".to_string(), json!(true));
    assert!(AgentResult::from_dict(&unknown).is_err());
}
