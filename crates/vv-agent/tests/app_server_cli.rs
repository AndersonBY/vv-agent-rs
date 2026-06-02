use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use vv_agent::cli::{
    parse_cli_args_from_with_default_settings, parse_cli_command_from_with_default_settings,
    AppServerCliCommand, CliCommand, DebugCliCommand,
};

#[test]
fn cli_parses_app_server_stdio_command() {
    let command = parse_cli_command_from_with_default_settings(
        ["vv-agent", "app-server", "--listen", "stdio"],
        "local_settings.json",
    )
    .expect("parse command");

    assert_eq!(
        command,
        CliCommand::AppServer(AppServerCliCommand::ListenStdio)
    );
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
            "generate-json-schema",
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
            "generate-json-schema",
            "--out",
            json_out.to_str().expect("json path"),
        ])
        .status()
        .expect("run json schema command");
    assert!(json_status.success());
    assert!(json_out.join("ClientRequest.json").exists());
    assert!(json_out.join("ServerNotification.json").exists());
    assert!(json_out.join("ServerRequest.json").exists());
    assert!(json_out.join("JsonRpcMessage.json").exists());

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

    let client_request =
        fs::read_to_string(json_out.join("ClientRequest.json")).expect("schema file");
    assert!(client_request.contains("thread/start"));
}

#[test]
fn cli_app_server_stdio_can_be_spawned_and_initialized() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_vv-agent"))
        .args(["app-server", "--listen", "stdio"])
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
    assert_eq!(response["result"]["serverInfo"]["name"], "vv-agent-rs");
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
