use serde_json::json;
use vv_agent::runtime::{normalize_token_usage, summarize_task_token_usage};
use vv_agent::{CycleRecord, LLMResponse, TokenUsage};

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
