use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::thread;
use std::time::Duration;

use vv_agent::{
    build_vv_llm_from_local_settings, load_llm_settings_from_file, resolve_model_endpoint,
    CacheUsageStatus, LlmClient, LlmRequest, Message, UsageSource,
};

#[test]
fn settings_builder_returns_vv_llm_backed_client() {
    let mut settings_file = tempfile::Builder::new()
        .suffix(".json")
        .tempfile()
        .expect("settings file");
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
                "kimi-k3": {{
                  "id": "kimi-k3",
                  "endpoints": [
                    {{
                      "endpoint_id": "moonshot-default",
                      "model_id": "kimi-k3"
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
        build_vv_llm_from_local_settings(settings_file.path(), "moonshot", "kimi-k3", 90.0)
            .expect("build llm");

    assert_eq!(resolved.backend, "moonshot");
    assert_eq!(resolved.requested_model, "kimi-k3");
    assert_eq!(resolved.selected_model, "kimi-k3");
    assert_eq!(resolved.model_id, "kimi-k3");
    assert_eq!(resolved.context_length, Some(128_000));
    assert!(resolved.function_call_available);
    assert!(resolved.response_format_available);
    assert_eq!(resolved.max_output_tokens, Some(16_384));
    assert_eq!(resolved.endpoint().unwrap().endpoint_id, "moonshot-default");
    assert_eq!(client.provider_name(), "openai-compatible");
    assert_eq!(client.model_id(), "kimi-k3");
}

#[test]
fn moonshot_stream_normalizes_omitted_cache_usage_as_observed_zero() {
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
            "model": "kimi-k3",
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
            "model": "kimi-k3",
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
            "model": "kimi-k3",
            "choices": [],
            "usage": {
                "prompt_tokens": 11,
                "completion_tokens": 7,
                "total_tokens": 18
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

    let mut settings_file = tempfile::Builder::new()
        .suffix(".json")
        .tempfile()
        .expect("settings file");
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
                        "kimi-k3": {
                            "id": "kimi-k3",
                            "endpoints": [{
                                "endpoint_id": "moonshot-default",
                                "model_id": "kimi-k3"
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
        build_vv_llm_from_local_settings(settings_file.path(), "moonshot", "kimi-k3", 5.0)
            .expect("build llm");
    let response = client
        .complete(LlmRequest::new("kimi-k3", vec![Message::user("hello")]))
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
    assert_eq!(response.token_usage.input_tokens, Some(11));
    assert_eq!(response.token_usage.output_tokens, Some(7));
    assert_eq!(response.token_usage.total_tokens, Some(18));
    assert_eq!(response.token_usage.cache_usage.read_input_tokens, Some(0));
    assert_eq!(
        response.token_usage.cache_usage.uncached_input_tokens,
        Some(11)
    );
    assert!(response
        .token_usage
        .provider_usage
        .get("prompt_tokens_details")
        .is_none());
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
    let mut settings_file = tempfile::Builder::new()
        .suffix(".json")
        .tempfile()
        .expect("settings file");
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
            "api_key": "sk-deepseek-test-key",
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
fn model_settings_contract_fixture_is_enforced() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/parity/model_settings.json"))
            .expect("model settings fixture");
    let canonical = &fixture["canonical_settings"];

    let resolved = resolve_model_endpoint(canonical, "openai", "contract-model")
        .expect("resolve canonical contract model");
    assert_eq!(resolved.model_id, "contract-model");
    assert_eq!(resolved.context_length, Some(1_000_000));
    assert_eq!(resolved.max_output_tokens, Some(100_000));

    for case in fixture["invalid_settings"]
        .as_array()
        .expect("invalid settings")
    {
        let error = resolve_model_endpoint(&case["settings"], "openai", "contract-model")
            .expect_err("invalid settings must fail");
        assert!(
            !error.to_string().trim().is_empty(),
            "invalid case {} must return a diagnostic",
            case["name"]
        );
    }
}

#[test]
fn settings_loader_rejects_unknown_extension_and_assignment() {
    let mut unknown_extension = tempfile::Builder::new()
        .suffix(".yaml")
        .tempfile()
        .expect("unknown extension file");
    write!(unknown_extension, "{{}}").expect("write unknown extension");
    let extension_error = load_llm_settings_from_file(unknown_extension.path())
        .expect_err("unknown extension must fail");
    assert!(extension_error
        .to_string()
        .contains("unsupported settings file extension"));

    let mut wrong_assignment = tempfile::Builder::new()
        .suffix(".py")
        .tempfile()
        .expect("python settings file");
    writeln!(wrong_assignment, "MODEL_SETTINGS = {{}}").expect("write alternate assignment");
    let assignment_error = load_llm_settings_from_file(wrong_assignment.path())
        .expect_err("alternate assignment must fail");
    assert!(assignment_error.to_string().contains("LLM_SETTINGS"));
}

#[test]
fn settings_loader_accepts_checked_in_dev_settings_example_json_fixture() {
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/dev_settings.example.json");

    let settings = load_llm_settings_from_file(&example).expect("load vv-llm fixture settings");
    let resolved =
        resolve_model_endpoint(&settings, "moonshot", "kimi-k3").expect("resolve fixture model");

    assert_eq!(resolved.backend, "moonshot");
    assert_eq!(resolved.model_id, "kimi-k3");
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
        resolve_model_endpoint(&settings, "moonshot", "kimi-k3").expect("resolve moonshot");

    assert_eq!(deepseek.backend, "deepseek");
    assert_eq!(deepseek.model_id, "deepseek-v4-pro");
    assert_eq!(moonshot.backend, "moonshot");
    assert_eq!(moonshot.model_id, "kimi-k3");
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
