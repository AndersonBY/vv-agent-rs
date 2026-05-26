use std::io::Write;

use vv_agent::{
    build_openai_llm_from_local_settings, build_vv_llm_from_local_settings, decode_api_key,
    resolve_model_endpoint,
};

#[test]
fn settings_builder_returns_vv_llm_backed_client() {
    let mut settings_file = tempfile::NamedTempFile::new().expect("settings file");
    write!(
        settings_file,
        r#"{{
          "VERSION": "2",
          "endpoints": [
            {{
              "id": "moonshot-default",
              "api_base": "https://api.moonshot.cn/v1",
              "api_key": "sk-test"
            }}
          ],
          "backends": {{
            "moonshot": {{
              "models": {{
                "kimi-k2.5": {{
                  "id": "kimi-k2-thinking",
                  "endpoints": [
                    {{
                      "endpoint_id": "moonshot-default",
                      "model_id": "kimi-k2-thinking"
                    }}
                  ],
                  "context_length": 128000,
                  "max_output_tokens": 16384,
                  "function_call_available": true,
                  "response_format_available": true
                }}
              }}
            }}
          }},
          "embedding_backends": {{}},
          "rerank_backends": {{}}
        }}"#
    )
    .expect("write settings");

    let (client, resolved) =
        build_vv_llm_from_local_settings(settings_file.path(), "moonshot", "kimi-k2.5", 90.0)
            .expect("build llm");

    assert_eq!(resolved.backend, "moonshot");
    assert_eq!(resolved.requested_model, "kimi-k2.5");
    assert_eq!(resolved.selected_model, "kimi-k2.5");
    assert_eq!(resolved.model_id, "kimi-k2-thinking");
    assert_eq!(resolved.endpoint().unwrap().endpoint_id, "moonshot-default");
    assert_eq!(client.provider_name(), "openai-compatible");
    assert_eq!(client.model_id(), "kimi-k2-thinking");
}

#[test]
fn legacy_openai_named_builder_still_delegates_to_vv_llm_builder() {
    let mut settings_file = tempfile::NamedTempFile::new().expect("settings file");
    write!(
        settings_file,
        r#"{{
          "VERSION": "2",
          "endpoints": [
            {{
              "id": "deepseek-default",
              "api_base": "https://api.deepseek.com",
              "api_key": "sk-test"
            }}
          ],
          "backends": {{
            "deepseek": {{
              "models": {{
                "deepseek-v4-pro": {{
                  "id": "deepseek-v4-pro",
                  "endpoints": ["deepseek-default"]
                }}
              }}
            }}
          }},
          "embedding_backends": {{}},
          "rerank_backends": {{}}
        }}"#
    )
    .expect("write settings");

    let (client, resolved) = build_openai_llm_from_local_settings(
        settings_file.path(),
        "deepseek",
        "deepseek-v4-pro",
        90.0,
    )
    .expect("build legacy alias");

    assert_eq!(resolved.backend, "deepseek");
    assert_eq!(resolved.model_id, "deepseek-v4-pro");
    assert_eq!(client.provider_name(), "openai-compatible");
}

#[test]
fn settings_resolution_accepts_embedded_llm_settings() {
    let raw = serde_json::json!({
        "LLM_SETTINGS": {
            "VERSION": "2",
            "endpoints": [{
                "id": "openai-default",
                "api_base": "https://api.openai.com/v1",
                "api_key": "sk-test"
            }],
            "backends": {
                "openai": {
                    "models": {
                        "gpt-4o": {
                            "id": "gpt-4o",
                            "endpoints": ["openai-default"]
                        }
                    }
                }
            },
            "embedding_backends": {},
            "rerank_backends": {}
        }
    });

    let resolved = resolve_model_endpoint(&raw, "openai", "gpt-4o").expect("resolve");

    assert_eq!(resolved.backend, "openai");
    assert_eq!(resolved.model_id, "gpt-4o");
    assert_eq!(
        resolved.endpoint().unwrap().api_base,
        "https://api.openai.com/v1"
    );
}

#[test]
fn settings_resolution_accepts_providers_alias_and_decodes_api_keys() {
    let raw = serde_json::json!({
        "VERSION": "2",
        "endpoints": [{
            "id": "deepseek-default",
            "api_base": "https://api.deepseek.com",
            "api_key": "env:sk-deepseek-test-key"
        }],
        "providers": {
            "deepseek": {
                "models": {
                    "deepseek-v4-pro": {
                        "id": "deepseek-v4-pro",
                        "endpoints": ["deepseek-default"]
                    }
                }
            }
        },
        "embedding_backends": {},
        "rerank_backends": {}
    });

    let resolved =
        resolve_model_endpoint(&raw, "deepseek", "deepseek-v4-pro").expect("resolve provider");

    assert_eq!(resolved.backend, "deepseek");
    assert_eq!(resolved.endpoint().unwrap().api_key, "sk-deepseek-test-key");
    assert_eq!(
        decode_api_key("env:sk-deepseek-test-key"),
        "sk-deepseek-test-key"
    );

    std::env::set_var("V_AGENT_ENABLE_BASE64_KEY_DECODE", "1");
    assert_eq!(
        decode_api_key("c2stZGVlcHNlZWstdGVzdC1rZXk"),
        "sk-deepseek-test-key"
    );
    std::env::remove_var("V_AGENT_ENABLE_BASE64_KEY_DECODE");
}
