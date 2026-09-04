use serde_json::{Map, Value};
use vv_agent::runtime::backends::distributed::CycleDispatchResult;
use vv_agent::runtime::checkpoint_codec::{checkpoint_from_value, checkpoint_to_value};
use vv_agent::{AgentResult, OperationJournalEntry, ToolExecutionResult};

const BOUNDED_FIXTURE: &str = include_str!("fixtures/parity/bounded_tool_result.json");
const RESULT_FIXTURE: &str = include_str!("fixtures/parity/result_public.json");
const JOURNAL_FIXTURE: &str = include_str!("fixtures/parity/operation_journal.json");
const CHECKPOINT_FIXTURE: &str = include_str!("fixtures/parity/checkpoint_codec.json");
const WORKER_FIXTURE: &str = include_str!("fixtures/parity/distributed_worker_response.json");

fn fixture(raw: &str) -> Value {
    serde_json::from_str(raw).expect("valid parity fixture")
}

fn object_at_mut<'a>(root: &'a mut Value, path: &[&str]) -> &'a mut Map<String, Value> {
    let mut cursor = root;
    for segment in path {
        cursor = cursor
            .as_object_mut()
            .expect("mutation path object")
            .get_mut(*segment)
            .expect("mutation path segment");
    }
    cursor.as_object_mut().expect("mutation target object")
}

fn set_dotted(root: &mut Value, dotted: &str, value: Value) {
    let mut path = dotted.split('.').collect::<Vec<_>>();
    let key = path.pop().expect("mutation field");
    object_at_mut(root, &path).insert(key.to_string(), value);
}

fn remove_dotted(root: &mut Value, dotted: &str) {
    let mut path = dotted.split('.').collect::<Vec<_>>();
    let key = path.pop().expect("mutation field");
    object_at_mut(root, &path).remove(key);
}

fn apply_mutation(result: &mut Value, mutation: &Value) {
    if let Some(field) = mutation.get("remove").and_then(Value::as_str) {
        remove_dotted(result, field);
    }
    for operation in ["add", "replace"] {
        let Some(fields) = mutation.get(operation).and_then(Value::as_object) else {
            continue;
        };
        for (field, value) in fields {
            set_dotted(result, field, value.clone());
        }
    }
}

#[test]
fn canonical_bounded_results_use_real_strict_readers_and_tool_projection() {
    let contract = fixture(BOUNDED_FIXTURE);
    let canonical = contract["canonical_results"]
        .as_object()
        .expect("canonical results");

    for (name, payload) in canonical {
        let result = ToolExecutionResult::from_dict(payload).expect(name);
        assert_eq!(result.to_dict(), *payload, "{name}");
        let serde_result: ToolExecutionResult =
            serde_json::from_value(payload.clone()).expect("strict serde reader");
        assert_eq!(serde_json::to_value(&serde_result).unwrap(), *payload);
    }

    for case in contract["tool_message_projection"]["cases"]
        .as_array()
        .expect("projection cases")
    {
        let result_ref = case["result_ref"].as_str().expect("result ref");
        let result = ToolExecutionResult::from_dict(&canonical[result_ref]).expect("result");
        assert_eq!(
            result.to_tool_message().content,
            case["expected_message"].as_str().expect("expected message"),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn bounded_result_fixture_mutations_are_rejected_by_the_real_reader() {
    let contract = fixture(BOUNDED_FIXTURE);
    let canonical = &contract["canonical_results"];

    for case in contract["invalid_cases"]
        .as_array()
        .expect("invalid cases")
        .iter()
        .filter(|case| {
            case.get("base").is_some()
                && case.get("mutation").is_some()
                && case["expected_error_code"] != "cursor_offset_invalid"
                && case["name"] != "success_result_has_non_null_error_code"
        })
    {
        let base = case["base"].as_str().expect("base result");
        let mut payload = canonical[base].clone();
        apply_mutation(&mut payload, &case["mutation"]);
        let error = ToolExecutionResult::from_dict(&payload).unwrap_err();
        let expected = case["expected_error_code"]
            .as_str()
            .expect("expected error code");
        assert!(
            error.contains(expected),
            "{} expected {expected}, got {error}",
            case["name"]
        );
    }

    for field in contract["result_contract"]["optional_fields"]
        .as_array()
        .expect("optional fields")
    {
        let field = field.as_str().expect("optional field");
        let mut payload = canonical["ordinary"].clone();
        payload[field] = Value::Null;
        assert!(
            ToolExecutionResult::from_dict(&payload).is_err(),
            "optional field {field} must reject null"
        );
    }
}

#[test]
fn bounded_success_error_code_is_rejected_by_deferred_validator() {
    let contract = fixture(BOUNDED_FIXTURE);
    let case = contract["invalid_cases"]
        .as_array()
        .expect("invalid cases")
        .iter()
        .find(|case| case["name"] == "success_result_has_non_null_error_code")
        .expect("success error-code case");
    let base = case["base"].as_str().expect("base result");
    let mut payload = contract["canonical_results"][base].clone();
    apply_mutation(&mut payload, &case["mutation"]);

    let result = ToolExecutionResult::from_dict(&payload).expect("wire reader result");
    let error = vv_agent::checkpoint::validate_definitive_result(&result)
        .expect_err("SUCCESS results with error codes are invalid");
    assert_eq!(error.code(), case["expected_error_code"]);
}

#[test]
fn sparse_artifact_fields_survive_every_durable_contract_reader() {
    let bounded = fixture(BOUNDED_FIXTURE);
    let expected = &bounded["canonical_results"]["truncated_bash"];

    let result_fixture = fixture(RESULT_FIXTURE);
    let result = AgentResult::from_dict(&result_fixture["agent_result"]).expect("AgentResult");
    assert_eq!(result.to_dict(), result_fixture["agent_result"]);
    assert_eq!(result.cycles[0].tool_results[3].to_dict(), *expected);

    let journal_fixture = fixture(JOURNAL_FIXTURE);
    let journal_payload = journal_fixture["valid_entries"]
        .as_array()
        .expect("journal entries")
        .iter()
        .find(|entry| entry["name"] == "tool_succeeded_truncated_bash")
        .expect("truncated journal entry")["entry"]
        .clone();
    let journal = OperationJournalEntry::from_value(&journal_payload).expect("journal reader");
    assert_eq!(journal.to_value(), journal_payload);
    assert_eq!(journal.result.as_ref(), Some(expected));

    let checkpoint_fixture = fixture(CHECKPOINT_FIXTURE);
    let checkpoint_payload = &checkpoint_fixture["canonical_checkpoint"];
    let checkpoint = checkpoint_from_value(checkpoint_payload, 262_144).expect("checkpoint reader");
    assert_eq!(
        checkpoint_to_value(&checkpoint, 262_144).expect("checkpoint writer"),
        *checkpoint_payload
    );
    assert_eq!(checkpoint.cycles[0].tool_results[0].to_dict(), *expected);

    let worker_fixture = fixture(WORKER_FIXTURE);
    for case_name in ["terminal_candidate", "terminal_replay"] {
        let response = worker_fixture["valid_cases"]
            .as_array()
            .expect("worker cases")
            .iter()
            .find(|case| case["name"] == case_name)
            .expect("worker response")["response"]
            .clone();
        let decoded = CycleDispatchResult::from_dict(&response).expect("worker reader");
        assert_eq!(decoded.to_dict(), response, "{case_name}");
        let encoded = serde_json::to_value(&decoded).expect("worker serde writer");
        assert_eq!(encoded, response, "{case_name}");
    }
}
