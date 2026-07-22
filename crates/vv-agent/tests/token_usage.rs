use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use vv_agent::runtime::{
    normalize_token_usage, normalize_token_usage_with_hints, summarize_task_token_usage,
};
use vv_agent::{
    CacheUsage, CacheUsageStatus, CycleRecord, LLMResponse, TaskTokenUsage, TokenUsage, UsageSource,
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
        for (index, observation) in case["cycles"]
            .as_array()
            .expect("cycle observations")
            .iter()
            .enumerate()
        {
            summary.add_cycle(
                (index + 1) as u32,
                TokenUsage {
                    total_tokens: Some(1),
                    usage_source: UsageSource::ProviderReported,
                    cache_usage: serde_json::from_value::<CacheUsage>(observation.clone())
                        .expect("cache observation"),
                    ..TokenUsage::default()
                },
            );
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
fn summarizes_task_token_usage_from_cycles() {
    let cycles = vec![
        CycleRecord {
            index: 2,
            assistant_message: String::new(),
            tool_calls: vec![],
            tool_results: vec![],
            memory_compacted: false,
            token_usage: TokenUsage {
                input_tokens: Some(10),
                output_tokens: Some(5),
                total_tokens: Some(15),
                usage_source: UsageSource::ProviderReported,
                ..TokenUsage::default()
            },
        },
        CycleRecord {
            index: 3,
            assistant_message: String::new(),
            tool_calls: vec![],
            tool_results: vec![],
            memory_compacted: false,
            token_usage: LLMResponse::new("empty").token_usage,
        },
    ];

    let summary = summarize_task_token_usage(&cycles);

    assert_eq!(summary.input_tokens, None);
    assert_eq!(summary.output_tokens, None);
    assert_eq!(summary.total_tokens, None);
    assert_eq!(summary.cycles.len(), 2);
    assert_eq!(summary.cycles[0].cycle_index, 2);
    assert_eq!(summary.cycles[1].cycle_index, 3);
}
