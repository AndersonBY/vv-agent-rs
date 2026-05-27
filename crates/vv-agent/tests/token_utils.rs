use serde_json::json;
use vv_agent::{
    memory::token_utils::{
        count_messages_tokens, count_tokens, estimate_tokens, resolve_model_token_limits,
        resolve_model_token_limits_from_file,
    },
    Message,
};

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
fn count_tokens_accepts_json_payload_like_python() {
    let payload = json!({"role": "user", "content": "hello"});
    let expected_payload = serde_json::to_string(&payload).expect("json payload");

    assert_eq!(
        count_tokens(&payload, "demo"),
        estimate_tokens(&expected_payload, "demo")
    );
}

#[test]
fn count_messages_tokens_fallback_uses_openai_message_payload_like_python() {
    let mut message = Message::user("look");
    message.image_url = Some("https://example.test/image.png".to_string());
    let payload = vec![message.to_openai_message(true)];
    let expected_payload = serde_json::to_string(&payload).expect("message payload");

    assert_eq!(
        count_messages_tokens(&[message], "unknown-provider-model"),
        estimate_tokens(&expected_payload, "unknown-provider-model")
    );
}

#[test]
fn estimate_tokens_handles_cjk_and_ascii_mix_like_python() {
    assert_eq!(estimate_tokens("你好", "demo"), 3);
    assert_eq!(estimate_tokens("hello", "demo"), 1);
    assert_eq!(estimate_tokens("你好hello", "demo"), 4);
}

#[test]
fn resolve_model_token_limits_reads_vv_llm_settings_model_config() {
    let settings = json!({
        "VERSION": "2",
        "endpoints": [{"id": "openai-default", "api_base": "https://example.test", "api_key": "sk-test"}],
        "backends": {
            "openai": {
                "models": {
                    "gpt-demo": {
                        "id": "provider-gpt-demo",
                        "endpoints": ["openai-default"],
                        "context_length": 64000,
                        "max_output_tokens": 8000
                    }
                }
            }
        }
    });

    assert_eq!(
        resolve_model_token_limits(&settings, "openai", "gpt-demo"),
        (Some(64_000), Some(8_000))
    );
    assert_eq!(
        resolve_model_token_limits(&settings, "openai", "provider-gpt-demo"),
        (Some(64_000), Some(8_000))
    );
    assert_eq!(
        resolve_model_token_limits(&settings, "openai", "missing"),
        (None, None)
    );
}

#[test]
fn resolve_model_token_limits_from_file_accepts_json_settings_like_python_loader() {
    let settings = tempfile::NamedTempFile::new().expect("settings file");
    std::fs::write(
        settings.path(),
        r#"{
          "VERSION": "2",
          "endpoints": [{"id": "deepseek-default", "api_base": "https://example.test", "api_key": "sk-test"}],
          "backends": {
            "deepseek": {
              "models": {
                "deepseek-v4-pro": {
                  "id": "deepseek-v4-pro",
                  "endpoints": ["deepseek-default"],
                  "context_length": 131072,
                  "max_output_tokens": 8192
                }
              }
            }
          }
        }"#,
    )
    .expect("write settings");

    assert_eq!(
        resolve_model_token_limits_from_file(settings.path(), "deepseek", "deepseek-v4-pro"),
        (Some(131_072), Some(8_192))
    );
}
