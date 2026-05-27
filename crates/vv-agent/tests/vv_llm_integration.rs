use std::io::Write;

use vv_agent::{
    build_openai_llm_from_local_settings, build_vv_llm_from_local_settings, build_vv_llm_settings,
    decode_api_key, load_llm_settings_from_file, resolve_model_endpoint,
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
    assert_eq!(resolved.context_length, Some(128_000));
    assert_eq!(resolved.max_output_tokens, Some(16_384));
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
fn settings_resolution_collects_all_endpoint_options_like_python() {
    let raw = serde_json::json!({
        "VERSION": "2",
        "endpoints": [
            {
                "id": "deepseek-default",
                "api_base": "https://api.deepseek.com",
                "api_key": "sk-default"
            },
            {
                "id": "deepseek-backup",
                "api_base": "https://backup.deepseek.com",
                "api_key": "sk-backup"
            }
        ],
        "backends": {
            "deepseek": {
                "models": {
                    "deepseek-v4-pro": {
                        "id": "deepseek-v4-pro",
                        "endpoints": [
                            {"endpoint_id": "deepseek-default", "model_id": "deepseek-v4-pro"},
                            {"endpoint_id": "deepseek-backup", "model_id": "deepseek-chat"}
                        ]
                    }
                }
            }
        },
        "embedding_backends": {},
        "rerank_backends": {}
    });

    let resolved =
        resolve_model_endpoint(&raw, "deepseek", "deepseek-v4-pro").expect("resolve model");

    let endpoint_options = resolved
        .endpoint_options
        .iter()
        .map(|option| {
            (
                option.endpoint.endpoint_id.as_str(),
                option.endpoint.api_key.as_str(),
                option.model_id.as_str(),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        endpoint_options,
        vec![
            ("deepseek-default", "sk-default", "deepseek-v4-pro"),
            ("deepseek-backup", "sk-backup", "deepseek-chat"),
        ]
    );
}

#[test]
fn build_vv_llm_settings_normalizes_provider_aliases_keys_and_endpoint_options_like_python() {
    let raw = serde_json::json!({
        "endpoints": [
            {
                "id": "deepseek-default",
                "api_base": "https://api.deepseek.com",
                "api_key": "env:sk-default-key"
            },
            {
                "id": "deepseek-backup",
                "api_base": "https://backup.deepseek.com",
                "api_key": "env:sk-backup-key"
            }
        ],
        "providers": {
            "deepseek": {
                "models": {
                    "deepseek-v4-pro": {
                        "id": "deepseek-v4-pro",
                        "endpoints": [
                            {"endpoint_id": "deepseek-default", "model_id": "deepseek-v4-pro"},
                            {"endpoint_id": "deepseek-backup", "model_id": "deepseek-chat"}
                        ]
                    }
                }
            }
        },
        "embedding_backends": {},
        "rerank_backends": {}
    });
    let resolved =
        resolve_model_endpoint(&raw, "deepseek", "deepseek-v4-pro").expect("resolve model");

    let vv_settings =
        build_vv_llm_settings(&raw, "deepseek", &resolved).expect("build vv settings");

    assert_eq!(vv_settings.version.as_deref(), Some("2"));
    assert!(vv_settings.backends.contains_key("deepseek"));
    let backend = vv_settings.backends.get("deepseek").expect("backend");
    assert_eq!(backend.extra["default_endpoint"], "deepseek-default");
    let model = backend
        .models
        .get("deepseek-v4-pro")
        .expect("model setting");
    let endpoint_pairs = model
        .endpoints
        .iter()
        .map(|binding| {
            (
                binding.endpoint_id().to_string(),
                binding.model_id(&model.id).to_string(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        endpoint_pairs,
        vec![
            (
                "deepseek-default".to_string(),
                "deepseek-v4-pro".to_string()
            ),
            ("deepseek-backup".to_string(), "deepseek-chat".to_string()),
        ]
    );
    assert_eq!(
        vv_settings
            .endpoints
            .iter()
            .find(|endpoint| endpoint.id == "deepseek-default")
            .and_then(|endpoint| endpoint.api_key.as_deref()),
        Some("sk-default-key")
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

#[test]
fn settings_loader_accepts_python_llm_settings_literal() {
    let mut settings_file = tempfile::Builder::new()
        .suffix(".py")
        .tempfile()
        .expect("settings file");
    write!(
        settings_file,
        r#"
from vv_llm.types import SettingsDict

LLM_SETTINGS: SettingsDict = {{
    "VERSION": "2",
    "rate_limit": {{"enabled": False}},
    "endpoints": [
        {{
            # Comments can contain unmatched literal delimiters like {{
            "id": "deepseek-default",
            "api_base": "https://api.deepseek.com",
            "api_key": "env:sk-deepseek-test-key",
            "response_api": True,
        }},
    ],
    "backends": {{
        "deepseek": {{
            "models": {{
                "deepseek-v4-pro": {{
                    "id": "deepseek-v4-pro",
                    "endpoints": ["deepseek-default"],
                    "max_output_tokens": None,
                }},
            }},
        }},
    }},
    "embedding_backends": {{}},
    "rerank_backends": {{}},
}}
"#
    )
    .expect("write settings");

    let settings = load_llm_settings_from_file(settings_file.path()).expect("load python settings");
    assert_eq!(
        settings["rate_limit"]["enabled"],
        serde_json::Value::Bool(false)
    );
    assert_eq!(
        settings["backends"]["deepseek"]["models"]["deepseek-v4-pro"]["max_output_tokens"],
        serde_json::Value::Null
    );

    let resolved =
        resolve_model_endpoint(&settings, "deepseek", "deepseek-v4-pro").expect("resolve");
    assert_eq!(resolved.endpoint().unwrap().api_key, "sk-deepseek-test-key");
}

#[test]
fn settings_loader_accepts_lowercase_settings_template_literal() {
    let mut settings_file = tempfile::Builder::new()
        .suffix(".py")
        .tempfile()
        .expect("settings file");
    write!(
        settings_file,
        r#"
from vv_llm.types import SettingsDict

settings: SettingsDict = {{
    "VERSION": "2",
    "endpoints": [
        {{
            "id": "moonshot-default",
            "api_base": "https://api.moonshot.cn/v1",
            "api_key": "sk-moonshot-test-key",
        }},
    ],
    "backends": {{
        "moonshot": {{
            "models": {{
                "kimi-k2-thinking": {{
                    "id": "kimi-k2-thinking",
                    "endpoints": [
                        {{"endpoint_id": "moonshot-default", "model_id": "kimi-k2-thinking"}},
                    ],
                }},
            }},
        }},
    }},
    "embedding_backends": {{}},
    "rerank_backends": {{}},
}}
"#
    )
    .expect("write settings");

    let settings =
        load_llm_settings_from_file(settings_file.path()).expect("load lowercase settings");
    let resolved =
        resolve_model_endpoint(&settings, "moonshot", "kimi-k2-thinking").expect("resolve");

    assert_eq!(resolved.endpoint().unwrap().endpoint_id, "moonshot-default");
    assert_eq!(resolved.model_id, "kimi-k2-thinking");
}
