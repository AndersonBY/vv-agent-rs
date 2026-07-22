use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use vv_agent::cli::{
    parse_cli_args_from_with_default_settings, parse_cli_command_from_with_default_settings,
    AppServerCliCommand, CliCommand, DebugCliCommand,
};

const JSON_SCHEMA_NAMES: &[&str] = &[
    "AppItem",
    "AppThread",
    "AppTurn",
    "ApprovalDecision",
    "ApprovalRequestParams",
    "ApprovalResolveParams",
    "ClientRequest",
    "InitializeParams",
    "InitializeResponse",
    "JsonRpcMessage",
    "SchemaExportResponse",
    "ServerNotification",
    "ServerRequest",
    "ThreadReadResponse",
    "ThreadResumeResponse",
    "ThreadStartResponse",
    "TurnResumeParams",
    "TurnResumeResponse",
    "TurnStartResponse",
];

#[test]
fn cli_parses_app_server_stdio_command() {
    let command = parse_cli_command_from_with_default_settings(
        [
            "vv-agent",
            "app-server",
            "--model=test-model",
            "--timeout-seconds=12.5",
            "--settings",
            "settings.json",
            "--listen=stdio",
            "--backend=test-backend",
        ],
        "local_settings.json",
    )
    .expect("parse command");

    assert_eq!(
        command,
        CliCommand::AppServer(AppServerCliCommand::ListenStdio {
            settings_file: "settings.json".into(),
            backend: "test-backend".to_string(),
            model: "test-model".to_string(),
            timeout_seconds: 12.5,
        })
    );
}

#[test]
fn cli_app_server_listener_rejects_missing_duplicate_unknown_and_invalid_arguments() {
    let valid = [
        "vv-agent",
        "app-server",
        "--listen",
        "stdio",
        "--settings",
        "settings.json",
        "--backend",
        "test-backend",
        "--model",
        "test-model",
    ];
    let cases = vec![
        (
            vec![
                "vv-agent",
                "app-server",
                "--settings",
                "settings.json",
                "--backend",
                "test-backend",
                "--model",
                "test-model",
            ],
            "requires --listen",
        ),
        (
            vec![
                "vv-agent",
                "app-server",
                "--listen",
                "stdio",
                "--backend",
                "test-backend",
                "--model",
                "test-model",
            ],
            "requires --settings",
        ),
        (
            vec![
                "vv-agent",
                "app-server",
                "--listen",
                "stdio",
                "--settings",
                "settings.json",
                "--model",
                "test-model",
            ],
            "requires --backend",
        ),
        (
            vec![
                "vv-agent",
                "app-server",
                "--listen",
                "stdio",
                "--settings",
                "settings.json",
                "--backend",
                "test-backend",
            ],
            "requires --model",
        ),
        (
            vec![
                "vv-agent",
                "app-server",
                "--listen",
                "stdio",
                "--listen=stdio",
                "--settings",
                "settings.json",
                "--backend",
                "test-backend",
                "--model",
                "test-model",
            ],
            "duplicate app-server argument: --listen",
        ),
        (
            vec![
                "vv-agent",
                "app-server",
                "--listen",
                "stdio",
                "--settings",
                "settings.json",
                "--settings=other.json",
                "--backend",
                "test-backend",
                "--model",
                "test-model",
            ],
            "duplicate app-server argument: --settings",
        ),
        (
            vec![
                "vv-agent",
                "app-server",
                "--listen",
                "stdio",
                "--settings",
                "settings.json",
                "--backend",
                "test-backend",
                "--backend=other",
                "--model",
                "test-model",
            ],
            "duplicate app-server argument: --backend",
        ),
        (
            vec![
                "vv-agent",
                "app-server",
                "--listen",
                "stdio",
                "--settings",
                "settings.json",
                "--backend",
                "test-backend",
                "--model",
                "test-model",
                "--model=other",
            ],
            "duplicate app-server argument: --model",
        ),
        (
            valid
                .into_iter()
                .chain(["--timeout-seconds", "1", "--timeout-seconds=2"])
                .collect(),
            "duplicate app-server argument: --timeout-seconds",
        ),
        (
            valid
                .into_iter()
                .chain(["--workspace", "./workspace"])
                .collect(),
            "unknown app-server argument: --workspace",
        ),
        (
            valid.into_iter().chain(["trailing"]).collect(),
            "unknown app-server argument: trailing",
        ),
    ];

    for (args, expected) in cases {
        let error = parse_cli_command_from_with_default_settings(args, "local_settings.json")
            .expect_err("invalid App Server arguments must fail");
        assert!(
            error.contains(expected),
            "expected {expected:?} in {error:?}"
        );
    }
}

#[test]
fn cli_app_server_listener_rejects_missing_values_and_non_positive_or_non_finite_timeouts() {
    for value in ["0", "-1", "NaN", "inf", "-inf", "1e999", "not-a-number"] {
        let error = parse_cli_command_from_with_default_settings(
            [
                "vv-agent",
                "app-server",
                "--listen",
                "stdio",
                "--settings",
                "settings.json",
                "--backend",
                "test-backend",
                "--model",
                "test-model",
                "--timeout-seconds",
                value,
            ],
            "local_settings.json",
        )
        .expect_err("invalid timeout must fail");
        assert!(
            error.contains("--timeout-seconds must be a finite positive number"),
            "unexpected error for {value:?}: {error}"
        );
    }

    for flag in [
        "--listen",
        "--settings",
        "--backend",
        "--model",
        "--timeout-seconds",
    ] {
        let error = parse_cli_command_from_with_default_settings(
            ["vv-agent", "app-server", flag],
            "local_settings.json",
        )
        .expect_err("missing value must fail");
        assert!(error.contains(&format!("{flag} requires a value")));
    }
}

#[test]
fn cli_app_server_timeout_defaults_to_ninety_seconds() {
    let command = parse_cli_command_from_with_default_settings(
        [
            "vv-agent",
            "app-server",
            "--listen",
            "stdio",
            "--settings",
            "settings.json",
            "--backend",
            "test-backend",
            "--model",
            "test-model",
        ],
        "local_settings.json",
    )
    .expect("parse command");

    assert!(matches!(
        command,
        CliCommand::AppServer(AppServerCliCommand::ListenStdio {
            timeout_seconds: 90.0,
            ..
        })
    ));
}

#[test]
fn cli_app_server_schema_commands_reject_trailing_arguments() {
    for command in ["generate-ts", "schema"] {
        let error = parse_cli_command_from_with_default_settings(
            [
                "vv-agent",
                "app-server",
                command,
                "--out",
                "target/schema",
                "trailing",
            ],
            "local_settings.json",
        )
        .expect_err("trailing schema argument must fail");
        assert!(error.contains("requires --out <dir>"));
    }
}

#[test]
fn cli_parses_app_server_schema_generation_commands() {
    let ts = parse_cli_command_from_with_default_settings(
        [
            "vv-agent",
            "app-server",
            "generate-ts",
            "--out",
            "target/app-server-schema/typescript",
        ],
        "local_settings.json",
    )
    .expect("parse ts command");
    assert_eq!(
        ts,
        CliCommand::AppServer(AppServerCliCommand::GenerateTs {
            out: "target/app-server-schema/typescript".into()
        })
    );

    let json = parse_cli_command_from_with_default_settings(
        [
            "vv-agent",
            "app-server",
            "schema",
            "--out",
            "target/app-server-schema/json",
        ],
        "local_settings.json",
    )
    .expect("parse json command");
    assert_eq!(
        json,
        CliCommand::AppServer(AppServerCliCommand::GenerateJsonSchema {
            out: "target/app-server-schema/json".into()
        })
    );
}

#[test]
fn production_cli_does_not_use_test_only_app_server_constructor() {
    let source = include_str!("../src/cli.rs");

    assert!(!source.contains("MessageProcessor::new_for_tests"));
    assert!(source.contains("FixedAppServerModelProvider"));
    assert!(!source.contains("let (_llm, resolved)"));
    assert!(source.contains("approval_timeout: Some(APP_SERVER_APPROVAL_TIMEOUT)"));
    assert!(source.contains("AppServer::new(StdioJsonlTransport::new(), processor, outgoing)"));
    assert!(!source.contains("tokio::select!"));
}

#[test]
fn cli_parses_debug_app_server_send_message_command() {
    let command = parse_cli_command_from_with_default_settings(
        ["vv-agent", "debug", "app-server", "send-message", "hello"],
        "local_settings.json",
    )
    .expect("parse debug command");

    assert_eq!(
        command,
        CliCommand::Debug(DebugCliCommand::AppServerSendMessage {
            message: "hello".to_string()
        })
    );
}

#[test]
fn existing_prompt_cli_parse_still_works() {
    let args = parse_cli_args_from_with_default_settings(
        ["vv-agent", "--prompt", "finish this"],
        "local_settings.json",
    )
    .expect("parse prompt");

    assert_eq!(args.prompt, "finish this");
}

#[test]
fn cli_schema_generation_writes_json_and_typescript_files() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let json_out = tempdir.path().join("json");
    let ts_out = tempdir.path().join("typescript");
    let json_status = Command::new(env!("CARGO_BIN_EXE_vv-agent"))
        .args([
            "app-server",
            "schema",
            "--out",
            json_out.to_str().expect("json path"),
        ])
        .status()
        .expect("run json schema command");
    assert!(json_status.success());
    assert_json_schema_file_set(&json_out);

    let ts_status = Command::new(env!("CARGO_BIN_EXE_vv-agent"))
        .args([
            "app-server",
            "generate-ts",
            "--out",
            ts_out.to_str().expect("ts path"),
        ])
        .status()
        .expect("run ts command");
    assert!(ts_status.success());
    assert!(ts_out.join("ClientRequest.ts").exists());
    assert!(ts_out.join("ServerNotification.ts").exists());
    assert!(ts_out.join("ServerRequest.ts").exists());
    assert!(ts_out.join("TurnResumeParams.ts").exists());
    assert!(ts_out.join("TurnResumeResponse.ts").exists());

    let client_request =
        fs::read_to_string(json_out.join("json/ClientRequest.json")).expect("schema file");
    assert!(client_request.contains("thread/start"));
    assert!(client_request.contains("turn/resume"));
    let aggregate: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(json_out.join("json/vv_agent_app_server.schemas.json"))
            .expect("aggregate schema file"),
    )
    .expect("aggregate schema JSON");
    let aggregate_names = aggregate
        .as_object()
        .expect("aggregate schema object")
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        aggregate_names,
        JSON_SCHEMA_NAMES.iter().copied().collect::<BTreeSet<_>>()
    );
    assert!(aggregate.get("TurnStartParams").is_none());
}

#[test]
fn cli_app_server_stdio_returns_parse_error_and_keeps_serving() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let mut child = configured_app_server_command(tempdir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn app server");
    let mut stdin = child.stdin.take().expect("stdin");
    writeln!(stdin, "{{not json").expect("write malformed json");
    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "initialize",
            "params": {"clientInfo": {"name": "after-parse-error"}}
        })
    )
    .expect("write initialize");

    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
    let mut first = String::new();
    let mut second = String::new();
    stdout.read_line(&mut first).expect("read parse error");
    stdout
        .read_line(&mut second)
        .expect("read initialize response");
    child.kill().ok();
    let _ = child.wait();

    let error: serde_json::Value = serde_json::from_str(&first).expect("parse error json");
    let response: serde_json::Value =
        serde_json::from_str(&second).expect("initialize response json");
    assert!(error["id"].is_null());
    assert_eq!(error["error"]["code"], -32700);
    assert_eq!(response["id"], 7);
    assert_eq!(response["result"]["protocolVersion"], "v1");
}

#[test]
fn cli_app_server_stdio_lists_the_selected_production_model() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let mut child = configured_app_server_command(tempdir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn app server");
    let mut stdin = child.stdin.take().expect("stdin");
    for payload in [
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"clientInfo": {"name": "async-stdio"}}
        }),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "model/list",
            "params": {}
        }),
    ] {
        writeln!(stdin, "{payload}").expect("write request");
    }

    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
    let mut initialize = String::new();
    let mut models = String::new();
    stdout
        .read_line(&mut initialize)
        .expect("read initialize response");
    stdout.read_line(&mut models).expect("read model list");
    child.kill().ok();
    let _ = child.wait();

    let initialize: serde_json::Value = serde_json::from_str(&initialize).expect("initialize json");
    let models: serde_json::Value = serde_json::from_str(&models).expect("model list json");
    assert_eq!(initialize["id"], 1);
    assert_eq!(models["id"], 2);
    assert_eq!(models["result"]["models"][0]["id"], "deepseek-v4-pro");
    assert_eq!(models["result"]["models"][0]["provider"], "deepseek");
    assert_eq!(models["result"]["models"][0]["contextLength"], 128_000);
    assert_eq!(models["result"]["models"][0]["supportsTools"], true);
    assert_eq!(
        models["result"]["models"][0]["metadata"]["requestedModel"],
        "deepseek-v4-pro"
    );
}

#[test]
fn cli_app_server_stdio_can_be_spawned_and_initialized() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let mut child = configured_app_server_command(tempdir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn app server");

    {
        let stdin = child.stdin.as_mut().expect("stdin");
        writeln!(
            stdin,
            "{}",
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "clientInfo": {"name": "stdio-test"},
                    "capabilities": {}
                }
            })
        )
        .expect("write initialize");
    }

    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
    let mut line = String::new();
    stdout.read_line(&mut line).expect("read response");
    child.kill().ok();
    let _ = child.wait();

    let response: serde_json::Value = serde_json::from_str(&line).expect("json response");
    assert_eq!(response["id"], 1);
    assert_eq!(response["result"]["userAgent"], "vv-agent-app-server");
    assert_eq!(response["result"]["protocolVersion"], "v1");
    assert_eq!(response["result"]["capabilities"]["threadLifecycle"], true);
}

#[test]
fn cli_app_server_resolves_settings_before_reading_stdio() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let missing = tempdir.path().join("missing.json");
    let mut child = Command::new(env!("CARGO_BIN_EXE_vv-agent"))
        .arg("app-server")
        .arg("--listen")
        .arg("stdio")
        .arg("--settings")
        .arg(&missing)
        .arg("--backend")
        .arg("missing-backend")
        .arg("--model")
        .arg("missing-model")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn app server");
    let _stdin = child.stdin.take().expect("keep stdin open");
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll app server") {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().ok();
            let _ = child.wait();
            panic!("App Server waited for stdin before resolving its settings");
        }
        thread::sleep(Duration::from_millis(10));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut stderr)
        .expect("read stderr");

    assert_eq!(status.code(), Some(2));
    assert!(stderr.contains(&format!("settings file not found: {}", missing.display())));
}

#[test]
fn cli_app_server_uses_two_for_usage_and_one_for_runtime_errors() {
    let usage = Command::new(env!("CARGO_BIN_EXE_vv-agent"))
        .args(["app-server", "--listen", "stdio"])
        .output()
        .expect("run usage error");
    assert_eq!(usage.status.code(), Some(2));

    let direct_task_usage = Command::new(env!("CARGO_BIN_EXE_vv-agent"))
        .output()
        .expect("run direct-task usage error");
    assert_eq!(direct_task_usage.status.code(), Some(1));

    let tempdir = tempfile::tempdir().expect("tempdir");
    let output_file = tempdir.path().join("not-a-directory");
    fs::write(&output_file, "occupied").expect("write output file");
    let runtime = Command::new(env!("CARGO_BIN_EXE_vv-agent"))
        .arg("app-server")
        .arg("schema")
        .arg("--out")
        .arg(output_file)
        .output()
        .expect("run runtime error");
    assert_eq!(runtime.status.code(), Some(1));
}

#[test]
fn cli_app_server_reuses_startup_model_after_settings_are_deleted() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let (api_base, server) = spawn_sse_completion_server();
    let settings_file = tempdir.path().join("settings.json");
    write_app_server_settings(&settings_file, &api_base);

    let mut child = Command::new(env!("CARGO_BIN_EXE_vv-agent"))
        .arg("app-server")
        .arg("--model=test-model")
        .arg("--settings")
        .arg(&settings_file)
        .arg("--timeout-seconds=5")
        .arg("--backend=deepseek")
        .arg("--listen=stdio")
        .current_dir(tempdir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn app server");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));

    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"clientInfo": {"name": "fixed-provider-test"}}
        })
    )
    .expect("write initialize");
    let initialized = read_json_until(&mut stdout, |message| message["id"] == 1);
    assert_eq!(initialized["result"]["protocolVersion"], "v1");

    fs::remove_file(&settings_file).expect("delete settings after startup");
    for payload in [
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialized"
        }),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "thread/start",
            "params": {"agentKey": "default", "metadata": {}}
        }),
    ] {
        writeln!(stdin, "{payload}").expect("write thread setup");
    }
    let thread_response = read_json_until(&mut stdout, |message| message["id"] == 2);
    let thread_id = thread_response["result"]["threadId"]
        .as_str()
        .expect("thread id")
        .to_string();

    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "turn/start",
            "params": {
                "threadId": thread_id,
                "input": [{"type": "text", "text": "finish without rereading settings"}]
            }
        })
    )
    .expect("write turn");
    let turn_started = read_json_until(&mut stdout, |message| message["id"] == 3);
    let request = server.join();

    child.kill().ok();
    let _ = child.wait();

    let request = request
        .expect("completion server")
        .expect("fixed provider never used its startup HTTP client");
    assert_eq!(turn_started["result"]["threadId"], thread_id);
    assert!(request.starts_with("POST /chat/completions "));
    assert!(request.contains("finish without rereading settings"));
}

#[test]
fn cli_debug_app_server_send_message_runs_scripted_turn() {
    let output = Command::new(env!("CARGO_BIN_EXE_vv-agent"))
        .args(["debug", "app-server", "send-message", "hello"])
        .output()
        .expect("run debug command");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("hello"));
    assert!(stdout.contains("turn/completed"));
}

fn configured_app_server_command(root: &Path) -> Command {
    let settings_file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("dev_settings.example.json");
    let mut command = Command::new(env!("CARGO_BIN_EXE_vv-agent"));
    command
        .arg("app-server")
        .arg("--listen")
        .arg("stdio")
        .arg("--settings")
        .arg(settings_file)
        .arg("--backend")
        .arg("deepseek")
        .arg("--model")
        .arg("deepseek-v4-pro")
        .current_dir(root);
    command
}

fn assert_json_schema_file_set(out: &Path) {
    let root_entries = fs::read_dir(out)
        .expect("schema output root")
        .map(|entry| {
            entry
                .expect("schema output entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(root_entries, BTreeSet::from(["json".to_string()]));

    let generated = fs::read_dir(out.join("json"))
        .expect("schema JSON directory")
        .map(|entry| {
            entry
                .expect("schema JSON entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<BTreeSet<_>>();
    let expected = JSON_SCHEMA_NAMES
        .iter()
        .map(|name| format!("{name}.json"))
        .chain(std::iter::once(
            "vv_agent_app_server.schemas.json".to_string(),
        ))
        .collect::<BTreeSet<_>>();
    assert_eq!(generated, expected);
}

fn read_json_until(
    reader: &mut impl BufRead,
    predicate: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).expect("read App Server output");
        assert_ne!(read, 0, "App Server stdout closed before expected message");
        let message = serde_json::from_str(&line).expect("App Server JSONL message");
        if predicate(&message) {
            return message;
        }
    }
}

fn write_app_server_settings(path: &Path, api_base: &str) {
    let settings = serde_json::json!({
        "VERSION": "2",
        "endpoints": [{
            "id": "test-endpoint",
            "api_base": api_base,
            "api_key": "sk-test"
        }],
        "backends": {
            "deepseek": {
                "models": {
                    "test-model": {
                        "id": "test-model",
                        "endpoints": [{
                            "endpoint_id": "test-endpoint",
                            "model_id": "test-model"
                        }],
                        "context_length": 128000,
                        "max_output_tokens": 8192,
                        "function_call_available": true,
                        "response_format_available": true
                    }
                }
            }
        },
        "embedding_backends": {},
        "rerank_backends": {}
    });
    fs::write(
        path,
        serde_json::to_vec(&settings).expect("serialize settings"),
    )
    .expect("write settings");
}

fn spawn_sse_completion_server() -> (String, thread::JoinHandle<Option<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind completion server");
    listener
        .set_nonblocking(true)
        .expect("set completion server nonblocking");
    let api_base = format!(
        "http://{}",
        listener.local_addr().expect("completion server address")
    );
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let (mut socket, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept completion request: {error}"),
            }
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set completion read timeout");
        let request = read_http_request(&mut socket);
        let chunk = serde_json::json!({
            "id": "chatcmpl-fixed-provider",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": "test-model",
            "choices": [{
                "index": 0,
                "delta": {"role": "assistant", "content": "settings snapshot survived"},
                "finish_reason": "stop"
            }]
        });
        let body = format!("data: {chunk}\n\ndata: [DONE]\n\n");
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n{:X}\r\n{}\r\n0\r\n\r\n",
            body.len(),
            body
        );
        socket
            .write_all(response.as_bytes())
            .expect("write completion response");
        socket.flush().expect("flush completion response");
        thread::sleep(Duration::from_millis(50));
        Some(request)
    });
    (api_base, server)
}

fn read_http_request(socket: &mut std::net::TcpStream) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let read = socket.read(&mut chunk).expect("read completion request");
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if http_request_is_complete(&buffer) {
            break;
        }
    }
    String::from_utf8(buffer).expect("completion request UTF-8")
}

fn http_request_is_complete(buffer: &[u8]) -> bool {
    let Some(header_end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let header = String::from_utf8_lossy(&buffer[..header_end]);
    let content_length = header
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(str::to_string)
        })
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    buffer.len() >= header_end + 4 + content_length
}
