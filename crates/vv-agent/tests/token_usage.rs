use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use vv_agent::runtime::{
    normalize_token_usage, normalize_token_usage_with_hints, summarize_task_token_usage,
};
use vv_agent::{
    CacheUsage, CacheUsageStatus, CycleRecord, LLMResponse, TaskTokenUsage, TokenUsage, UsageSource,
};

fn token_usage_contract() -> Value {
    serde_json::from_str(include_str!("fixtures/parity/token_usage_v1.json"))
        .expect("token usage contract fixture")
}

fn optional_hint<T: DeserializeOwned>(value: &Value) -> Option<T> {
    (!value.is_null()).then(|| serde_json::from_value(value.clone()).expect("valid hint"))
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
                    total_tokens: 1,
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
fn explicit_zero_usage_is_observable_and_old_payload_is_missing() {
    let explicit_zero = normalize_token_usage(&json!({
        "prompt_tokens": 0,
        "completion_tokens": 0,
        "total_tokens": 0,
        "prompt_tokens_details": {"cached_tokens": 0}
    }));
    let legacy =
        serde_json::from_value::<TokenUsage>(json!({"cached_tokens": 0})).expect("legacy usage");

    assert!(explicit_zero.has_usage());
    assert_eq!(explicit_zero.cache_usage.read_tokens, Some(0));
    assert!(!legacy.has_usage());
    assert_eq!(legacy.cache_usage.read_tokens, None);
}

#[test]
fn normalizes_provider_token_usage() {
    let usage = normalize_token_usage(&json!({
        "prompt_tokens": "11",
        "completion_tokens": 7.9,
        "prompt_tokens_details": {"cached_tokens": 3},
        "completion_tokens_details": {"reasoning_tokens": "5"},
        "input_tokens_details": {"cache_creation_tokens": 2}
    }));

    assert_eq!(usage.prompt_tokens, 11);
    assert_eq!(usage.completion_tokens, 7);
    assert_eq!(usage.input_tokens, 11);
    assert_eq!(usage.output_tokens, 7);
    assert_eq!(usage.total_tokens, 18);
    assert_eq!(usage.cached_tokens, 3);
    assert_eq!(usage.reasoning_tokens, 5);
    assert_eq!(usage.cache_creation_tokens, 2);
    assert_eq!(usage.raw["prompt_tokens"], "11");
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
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
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

    assert_eq!(summary.prompt_tokens, 10);
    assert_eq!(summary.completion_tokens, 5);
    assert_eq!(summary.total_tokens, 15);
    assert_eq!(summary.cycles.len(), 1);
    assert_eq!(summary.cycles[0].cycle_index, 2);
}
