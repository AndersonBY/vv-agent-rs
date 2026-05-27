use vv_agent::memory::token_utils::{count_tokens, estimate_tokens};

#[test]
fn count_tokens_prefers_vv_llm_counter_like_python() {
    let text = "hello world";
    let expected = vv_llm::utilities::count_tokens(text, "gpt-4o").expect("vv-llm count");

    assert_eq!(count_tokens(text, "gpt-4o"), expected as u64);
}

#[test]
fn count_tokens_returns_zero_for_empty_text_like_python() {
    assert_eq!(count_tokens("", "gpt-4o"), 0);
}

#[test]
fn count_tokens_falls_back_to_cjk_aware_estimate_for_unknown_models() {
    assert_eq!(
        count_tokens("你好hello", "demo"),
        estimate_tokens("你好hello", "demo")
    );
}

#[test]
fn estimate_tokens_handles_cjk_and_ascii_mix_like_python() {
    assert_eq!(estimate_tokens("你好", "demo"), 3);
    assert_eq!(estimate_tokens("hello", "demo"), 1);
    assert_eq!(estimate_tokens("你好hello", "demo"), 4);
}
