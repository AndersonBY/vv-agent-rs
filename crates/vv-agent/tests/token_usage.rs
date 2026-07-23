use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use vv_agent::runtime::{
    normalize_token_usage, normalize_token_usage_with_hints, summarize_task_token_usage,
};
use vv_agent::{
    CacheUsage, CacheUsageStatus, ModelCallOperation, ModelCallRecord, ModelCallStatus,
    TaskTokenUsage, TokenUsage, UsageSource,
};

fn token_usage_contract() -> Value {
    serde_json::from_str(include_str!("fixtures/parity/token_usage.json"))
        .expect("token usage contract fixture")
}

fn optional_hint<T: DeserializeOwned>(value: &Value) -> Option<T> {
    (!value.is_null()).then(|| serde_json::from_value(value.clone()).expect("valid hint"))
}

#[test]
fn task_token_usage_default_is_an_empty_aggregate() {
    let summary = TaskTokenUsage::default();
    let payload = serde_json::to_value(&summary).expect("serialized task token usage");

    assert_eq!(summary.cache_usage.source.as_deref(), Some("aggregate"));
    assert_eq!(payload["cache_usage"]["source"], json!("aggregate"));
    assert_eq!(
        serde_json::from_value::<TaskTokenUsage>(payload).expect("task token usage round trip"),
        summary
    );
}

#[test]
fn normalization_matches_canonical_token_usage_cases() {
    let contract = token_usage_contract();
    for case in contract["normalization_cases"]
        .as_array()
        .expect("normalization cases")
    {
        let input = &case["input"];
        let usage = normalize_token_usage_with_hints(
            &input["raw_usage"],
            optional_hint::<UsageSource>(&input["usage_source_hint"]),
            optional_hint::<CacheUsageStatus>(&input["cache_status_hint"]),
        );

        assert_eq!(
            serde_json::to_value(usage).expect("serialized token usage"),
            case["expected"],
            "{}",
            case["name"]
        );
    }
}

#[test]
fn aggregation_matches_canonical_cache_observation_cases() {
    let contract = token_usage_contract();
    for case in contract["aggregation_cases"]
        .as_array()
        .expect("aggregation cases")
    {
        let mut summary = TaskTokenUsage::default();
        for (index, observation) in case["model_calls"]
            .as_array()
            .expect("model call observations")
            .iter()
            .enumerate()
        {
            let cycle_index = (index + 1) as u32;
            summary
                .add_model_call(ModelCallRecord {
                    call_id: format!("op_model_cycle_{cycle_index}_main:attempt:1"),
                    operation_id: format!("op_model_cycle_{cycle_index}_main"),
                    attempt: 1,
                    operation: ModelCallOperation::AgentCycle,
                    cycle_index,
                    backend: "test".to_string(),
                    model: "test-model".to_string(),
                    status: ModelCallStatus::Completed,
                    usage: TokenUsage {
                        total_tokens: Some(1),
                        usage_source: UsageSource::ProviderReported,
                        cache_usage: serde_json::from_value::<CacheUsage>(observation.clone())
                            .expect("cache observation"),
                        ..TokenUsage::default()
                    },
                    error_code: None,
                })
                .expect("unique model call");
        }

        assert_eq!(
            serde_json::to_value(summary.cache_usage).expect("serialized cache aggregate"),
            case["expected"],
            "{}",
            case["name"]
        );
    }
}

#[test]
fn explicit_zero_usage_is_observable_and_superseded_wire_is_rejected() {
    let explicit_zero = normalize_token_usage(&json!({
        "prompt_tokens": 0,
        "completion_tokens": 0,
        "total_tokens": 0,
        "prompt_tokens_details": {"cached_tokens": 0}
    }));
    assert!(explicit_zero.has_usage());
    assert_eq!(explicit_zero.cache_usage.read_input_tokens, Some(0));
    assert!(serde_json::from_value::<TokenUsage>(json!({"cached_tokens": 0})).is_err());
}

#[test]
fn normalizes_provider_token_usage() {
    let usage = normalize_token_usage(&json!({
        "prompt_tokens": "11",
        "completion_tokens": 7,
        "prompt_tokens_details": {"cached_tokens": 3},
        "completion_tokens_details": {"reasoning_tokens": "5"},
        "input_tokens_details": {"cache_creation_tokens": 2}
    }));

    assert_eq!(usage.input_tokens, Some(11));
    assert_eq!(usage.output_tokens, Some(7));
    assert_eq!(usage.total_tokens, Some(18));
    assert_eq!(usage.reasoning_tokens, Some(5));
    assert_eq!(usage.cache_usage.read_input_tokens, Some(3));
    assert_eq!(usage.cache_usage.write_input_tokens, Some(2));
    assert_eq!(usage.provider_usage["prompt_tokens"], "11");
}

#[test]
fn task_aggregation_uses_complete_model_call_ledger() {
    let contract = token_usage_contract();
    let cases = contract["task_aggregation_cases"]
        .as_array()
        .expect("task aggregation cases");
    let expected_empty = &cases[0]["expected"];
    let empty = TaskTokenUsage::default();
    assert_eq!(empty.input_tokens, expected_empty["input_tokens"].as_u64());
    assert_eq!(
        empty.output_tokens,
        expected_empty["output_tokens"].as_u64()
    );
    assert_eq!(empty.total_tokens, expected_empty["total_tokens"].as_u64());
    assert_eq!(
        empty.reasoning_tokens,
        expected_empty["reasoning_tokens"].as_u64()
    );
    assert!(empty.model_calls.is_empty());

    let model_calls = cases[1]["model_calls"]
        .as_array()
        .expect("canonical model calls")
        .iter()
        .map(|value| {
            serde_json::from_value::<ModelCallRecord>(value.clone())
                .expect("canonical model call record")
        })
        .collect::<Vec<_>>();
    let summary = summarize_task_token_usage(&model_calls);
    let expected = &cases[1]["expected"];

    assert_eq!(summary.input_tokens, expected["input_tokens"].as_u64());
    assert_eq!(summary.output_tokens, expected["output_tokens"].as_u64());
    assert_eq!(summary.total_tokens, expected["total_tokens"].as_u64());
    assert_eq!(
        summary.reasoning_tokens,
        expected["reasoning_tokens"].as_u64()
    );
    assert_eq!(
        serde_json::to_value(&summary.cache_usage).expect("cache aggregate"),
        expected["cache_usage"]
    );
    assert_eq!(
        summary.model_calls.len() as u64,
        expected["model_call_count"]
            .as_u64()
            .expect("model call count")
    );
    assert_eq!(
        serde_json::from_value::<TaskTokenUsage>(
            serde_json::to_value(&summary).expect("task usage wire")
        )
        .expect("task usage round trip"),
        summary
    );
}

#[test]
fn duplicate_model_call_ids_and_superseded_task_wire_are_rejected() {
    let contract = token_usage_contract();
    let payload = contract["task_aggregation_cases"][1]["model_calls"][0].clone();
    let record = serde_json::from_value::<ModelCallRecord>(payload).expect("model call record");
    let mut summary = TaskTokenUsage::default();
    summary
        .add_model_call(record.clone())
        .expect("first model call");
    assert_eq!(
        summary.add_model_call(record),
        Err("model_call_id_duplicate".to_string())
    );

    let mut superseded = serde_json::to_value(summary)
        .expect("task usage")
        .as_object()
        .cloned()
        .expect("task usage object");
    superseded.insert(
        "schema_version".to_string(),
        Value::String("vv-agent.task-token-usage.v1".to_string()),
    );
    assert!(serde_json::from_value::<TaskTokenUsage>(Value::Object(superseded)).is_err());
}
