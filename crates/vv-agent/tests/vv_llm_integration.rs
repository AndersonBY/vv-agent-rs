use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::thread;
use std::time::Duration;

use vv_agent::{
    build_vv_llm_from_local_settings, build_vv_llm_settings, decode_api_key,
    load_llm_settings_from_file, load_memory_summary_defaults_from_file, resolve_model_endpoint,
    CacheUsageStatus, LlmClient, LlmRequest, Message, UsageSource,
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
                "kimi-k2.6": {{
                  "id": "kimi-k2.6",
                  "endpoints": [
                    {{
                      "endpoint_id": "moonshot-default",
                      "model_id": "kimi-k2.6"
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
        build_vv_llm_from_local_settings(settings_file.path(), "moonshot", "kimi-k2.6", 90.0)
            .expect("build llm");

    assert_eq!(resolved.backend, "moonshot");
    assert_eq!(resolved.requested_model, "kimi-k2.6");
    assert_eq!(resolved.selected_model, "kimi-k2.6");
    assert_eq!(resolved.model_id, "kimi-k2.6");
    assert_eq!(resolved.context_length, Some(128_000));
    assert!(resolved.function_call_available);
    assert!(resolved.response_format_available);
    assert_eq!(resolved.max_output_tokens, Some(16_384));
    assert_eq!(resolved.endpoint().unwrap().endpoint_id, "moonshot-default");
    assert_eq!(client.provider_name(), "openai-compatible");
    assert_eq!(client.model_id(), "kimi-k2.6");
}

#[test]
fn openai_compatible_stream_requests_and_reports_provider_cache_usage() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind completion server");
    let api_base = format!("http://{}", listener.local_addr().expect("server address"));
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept completion request");
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set read timeout");
        let request = read_http_request(&mut socket);
        let body = request.split("\r\n\r\n").nth(1).expect("request body");
        let payload: serde_json::Value = serde_json::from_str(body).expect("request JSON");

        assert!(request.starts_with("POST /chat/completions "));
        assert_eq!(payload["stream"], true);
        assert_eq!(
            payload["stream_options"],
            serde_json::json!({"include_usage": true})
        );

        let content_chunk = serde_json::json!({
            "id": "chatcmpl-agent-stream",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "kimi-k2.6",
            "choices": [{
                "index": 0,
                "delta": {"content": "ok"},
                "finish_reason": null
            }]
        });
        let finish_chunk = serde_json::json!({
            "id": "chatcmpl-agent-stream",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "kimi-k2.6",
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }],
            "usage": null
        });
        let usage_chunk = serde_json::json!({
            "id": "chatcmpl-agent-stream",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "kimi-k2.6",
            "choices": [],
            "usage": {
                "prompt_tokens": 11,
                "completion_tokens": 7,
                "total_tokens": 18,
                "prompt_tokens_details": {"cached_tokens": 6}
            }
        });
        let body = format!(
            "data: {content_chunk}\n\ndata: {finish_chunk}\n\ndata: {usage_chunk}\n\ndata: [DONE]\n\n"
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket
            .write_all(response.as_bytes())
            .expect("write completion response");
    });

    let mut settings_file = tempfile::NamedTempFile::new().expect("settings file");
    serde_json::to_writer(
        &mut settings_file,
        &serde_json::json!({
            "VERSION": "2",
            "endpoints": [{
                "id": "moonshot-default",
                "api_base": api_base,
                "api_key": "sk-test"
            }],
            "backends": {
                "moonshot": {
                    "models": {
                        "kimi-k2.6": {
                            "id": "kimi-k2.6",
                            "endpoints": [{
                                "endpoint_id": "moonshot-default",
                                "model_id": "kimi-k2.6"
                            }]
                        }
                    }
                }
            },
            "embedding_backends": {},
            "rerank_backends": {}
        }),
    )
    .expect("write settings");
    settings_file.flush().expect("flush settings");

    let (client, _) =
        build_vv_llm_from_local_settings(settings_file.path(), "moonshot", "kimi-k2.6", 5.0)
            .expect("build llm");
    let response = client
        .complete(LlmRequest::new("kimi-k2.6", vec![Message::user("hello")]))
        .expect("stream completion");
    server.join().expect("completion server");

    assert_eq!(response.content, "ok");
    assert_eq!(
        response.token_usage.usage_source,
        UsageSource::ProviderReported
    );
    assert_eq!(
        response.token_usage.cache_usage.status,
        CacheUsageStatus::ProviderReported
    );
    assert_eq!(response.token_usage.prompt_tokens, 11);
    assert_eq!(response.token_usage.completion_tokens, 7);
    assert_eq!(response.token_usage.total_tokens, 18);
    assert_eq!(response.token_usage.cached_tokens, 6);
    assert_eq!(response.token_usage.cache_usage.read_tokens, Some(6));
}

#[test]
fn settings_resolution_keeps_kimi_k25_and_k26_distinct() {
    let raw = serde_json::json!({
        "VERSION": "2",
        "endpoints": [
            {
                "id": "moonshot-default",
                "api_base": "https://api.moonshot.cn/v1",
                "api_key": "sk-test"
            }
        ],
        "backends": {
            "moonshot": {
                "models": {
                    "kimi-k2.5": {
                        "id": "kimi-k2.5",
                        "endpoints": [
                            {"endpoint_id": "moonshot-default", "model_id": "kimi-k2.5"}
                        ],
                        "context_length": 128000,
                        "max_output_tokens": 16384,
                        "function_call_available": true,
                        "response_format_available": true
                    },
                    "kimi-k2.6": {
                        "id": "kimi-k2.6",
                        "endpoints": [
                            {"endpoint_id": "moonshot-default", "model_id": "kimi-k2.6"}
                        ],
                        "context_length": 128000,
                        "max_output_tokens": 16384,
                        "function_call_available": true,
                        "response_format_available": true
                    }
                }
            }
        },
        "embedding_backends": {},
        "rerank_backends": {}
    });

    let k25 = resolve_model_endpoint(&raw, "moonshot", "kimi-k2.5").expect("resolve k2.5");
    let k26 = resolve_model_endpoint(&raw, "moonshot", "kimi-k2.6").expect("resolve k2.6");
    assert_eq!(k25.selected_model, "kimi-k2.5");
    assert_eq!(k25.model_id, "kimi-k2.5");
    assert_eq!(k26.selected_model, "kimi-k2.6");
    assert_eq!(k26.model_id, "kimi-k2.6");

    let k26_only = serde_json::json!({
        "VERSION": "2",
        "endpoints": [
            {
                "id": "moonshot-default",
                "api_base": "https://api.moonshot.cn/v1",
                "api_key": "sk-test"
            }
        ],
        "backends": {
            "moonshot": {
                "models": {
                    "kimi-k2.6": {
                        "id": "kimi-k2.6",
                        "endpoints": [
                            {"endpoint_id": "moonshot-default", "model_id": "kimi-k2.6"}
                        ],
                        "context_length": 128000,
                        "max_output_tokens": 16384,
                        "function_call_available": true,
                        "response_format_available": true
                    }
                }
            }
        },
        "embedding_backends": {},
        "rerank_backends": {}
    });
    let error =
        resolve_model_endpoint(&k26_only, "moonshot", "kimi-k2.5").expect_err("k2.5 is missing");
    assert!(
        error.to_string().contains("kimi-k2.5"),
        "missing model error should name the requested model: {error}"
    );
}

#[test]
fn vv_agent_does_not_embed_provider_protocol_clients() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src_dir = manifest_dir.join("src");
    let mut violations = Vec::new();

    for source_path in rust_source_files(&src_dir) {
        let relative_path = source_path
            .strip_prefix(manifest_dir)
            .unwrap_or(&source_path);
        let content = std::fs::read_to_string(&source_path).expect("read source file");
        for forbidden in [
            "async_openai",
            "anthropic::",
            "ChatCompletionRequest",
            "/chat/completions",
            "/messages",
        ] {
            if content.contains(forbidden) {
                violations.push(format!("{} contains {forbidden}", relative_path.display()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Provider protocol clients must stay in vv-llm, not vv-agent:\n{}",
        violations.join("\n")
    );
}

#[test]
fn vv_llm_builder_api_stays_vv_llm_focused() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(manifest_dir.join("src/config/model_resolution.rs"))
        .expect("read model resolution source");

    assert!(
        !source.contains("build_openai_llm_from_local_settings"),
        "vv-agent should expose the vv-llm builder name, not the legacy openai alias"
    );
    assert!(
        !std::fs::read_to_string(manifest_dir.join("src/config.rs"))
            .expect("read config module")
            .contains("build_openai_llm_from_local_settings"),
        "config module should not re-export the legacy openai builder alias"
    );
    assert!(
        !std::fs::read_to_string(manifest_dir.join("src/lib.rs"))
            .expect("read library module")
            .contains("build_openai_llm_from_local_settings"),
        "public lib exports should not surface the legacy openai builder alias"
    );
}

#[test]
fn examples_do_not_use_deprecated_kimi_k2_thinking_model() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let deprecated = ["kimi", "k2", "thinking"].join("-");
    let mut violations = Vec::new();

    for source_path in rust_source_files(&manifest_dir.join("src"))
        .into_iter()
        .chain(rust_source_files(&manifest_dir.join("tests")))
    {
        let relative_path = source_path
            .strip_prefix(manifest_dir)
            .unwrap_or(&source_path);
        let content = std::fs::read_to_string(&source_path).expect("read source file");
        if contains_exact_model_token(&content, &deprecated) {
            violations.push(relative_path.display().to_string());
        }
    }

    assert!(
        violations.is_empty(),
        "use kimi-k2.6 instead of the deprecated {deprecated} example model:\n{}",
        violations.join("\n")
    );
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
fn settings_resolution_collects_all_endpoint_options() {
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
fn settings_builder_uses_all_enabled_endpoint_options_for_failover() {
    let mut settings_file = tempfile::NamedTempFile::new().expect("settings file");
    write!(
        settings_file,
        r#"{{
          "VERSION": "2",
          "endpoints": [
            {{
              "id": "deepseek-default",
              "api_base": "https://api.deepseek.com",
              "api_key": "sk-default"
            }},
            {{
              "id": "deepseek-backup",
              "api_base": "https://backup.deepseek.com",
              "api_key": "sk-backup"
            }}
          ],
          "backends": {{
            "deepseek": {{
              "models": {{
                "deepseek-v4-pro": {{
                  "id": "deepseek-v4-pro",
                  "endpoints": [
                    {{"endpoint_id": "deepseek-default", "model_id": "deepseek-v4-pro"}},
                    {{"endpoint_id": "deepseek-backup", "model_id": "deepseek-chat"}}
                  ]
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
        build_vv_llm_from_local_settings(settings_file.path(), "deepseek", "deepseek-v4-pro", 90.0)
            .expect("build llm");

    assert_eq!(resolved.endpoint_options.len(), 2);
    assert_eq!(client.endpoint_count(), 2);
}

#[test]
fn build_vv_llm_settings_normalizes_provider_aliases_keys_and_endpoint_options() {
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
    assert_eq!(
        backend.default_endpoint.as_deref(),
        Some("deepseek-default")
    );
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
fn settings_loader_accepts_agent_llm_settings_literal() {
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

    let settings = load_llm_settings_from_file(settings_file.path()).expect("load settings");
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
                "kimi-k2.6": {{
                    "id": "kimi-k2.6",
                    "endpoints": [
                        {{"endpoint_id": "moonshot-default", "model_id": "kimi-k2.6"}},
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
    let resolved = resolve_model_endpoint(&settings, "moonshot", "kimi-k2.6").expect("resolve");

    assert_eq!(resolved.endpoint().unwrap().endpoint_id, "moonshot-default");
    assert_eq!(resolved.model_id, "kimi-k2.6");
}

#[test]
fn settings_loader_accepts_checked_in_dev_settings_example_json_fixture() {
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/dev_settings.example.json");

    let settings = load_llm_settings_from_file(&example).expect("load vv-llm fixture settings");
    let resolved =
        resolve_model_endpoint(&settings, "moonshot", "kimi-k2.6").expect("resolve fixture model");

    assert_eq!(resolved.backend, "moonshot");
    assert_eq!(resolved.model_id, "kimi-k2.6");
    assert!(resolved
        .endpoint()
        .unwrap()
        .api_base
        .starts_with("https://"));
    assert!(!resolved.endpoint().unwrap().api_key.trim().is_empty());
}

#[test]
fn settings_loader_accepts_vv_agent_dev_settings_example_json() {
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/dev_settings.example.json");

    let settings = load_llm_settings_from_file(&example).expect("load vv-agent fixture example");
    let deepseek =
        resolve_model_endpoint(&settings, "deepseek", "deepseek-v4-pro").expect("resolve deepseek");
    let moonshot =
        resolve_model_endpoint(&settings, "moonshot", "kimi-k2.6").expect("resolve moonshot");

    assert_eq!(deepseek.backend, "deepseek");
    assert_eq!(deepseek.model_id, "deepseek-v4-pro");
    assert_eq!(moonshot.backend, "moonshot");
    assert_eq!(moonshot.model_id, "kimi-k2.6");
}

#[test]
fn memory_summary_defaults_load_from_json_settings_file() {
    let mut settings_file = tempfile::NamedTempFile::new().expect("settings file");
    write!(
        settings_file,
        r#"{{
          "VV_AGENT_MEMORY_SUMMARY_BACKEND": "settings-backend",
          "VV_AGENT_MEMORY_SUMMARY_MODEL": "settings-model"
        }}"#
    )
    .expect("write settings");
    let summary_defaults = load_memory_summary_defaults_from_file(settings_file.path());
    assert_eq!(
        summary_defaults.backend.as_deref(),
        Some("settings-backend")
    );
    assert_eq!(summary_defaults.model.as_deref(), Some("settings-model"));
}

fn read_http_request(socket: &mut TcpStream) -> String {
    let mut reader = BufReader::new(socket);
    let mut request = String::new();
    let mut content_length = 0usize;

    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).expect("read request header");
        assert!(read > 0, "request ended before headers completed");
        request.push_str(&line);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line
            .strip_prefix("content-length:")
            .or_else(|| line.strip_prefix("Content-Length:"))
        {
            content_length = value.trim().parse().expect("content length");
        }
    }

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).expect("read request body");
    request.push_str(std::str::from_utf8(&body).expect("UTF-8 request body"));
    request
}

fn rust_source_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    collect_rust_source_files(root, &mut files);
    files
}

fn collect_rust_source_files(path: &Path, files: &mut Vec<std::path::PathBuf>) {
    let entries = std::fs::read_dir(path).expect("read source directory");
    for entry in entries {
        let entry = entry.expect("read source entry");
        let path = entry.path();
        if path.is_dir() {
            collect_rust_source_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

fn contains_exact_model_token(content: &str, model: &str) -> bool {
    content
        .split(|character: char| {
            !(character.is_ascii_alphanumeric()
                || character == '.'
                || character == '-'
                || character == '_'
                || character == '/')
        })
        .any(|token| token.rsplit('/').any(|segment| segment == model))
}
