use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use vv_agent::runtime::background_sessions::{
    background_session_manager, BackgroundSessionAdoptOptions, BackgroundSessionStartOptions,
};
use vv_agent::runtime::processes::{read_captured_output, start_captured_process};
use vv_agent::{build_default_registry, ToolCall, ToolContext, ToolResultStatus};

fn wait_for_background_payload<F>(description: &str, mut poll: F) -> Value
where
    F: FnMut() -> Value,
{
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let payload = poll();
        if payload["status"] != "running" {
            return payload;
        }
        if Instant::now() >= deadline {
            panic!("{description}: timed out waiting for background session: {payload}");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn wait_until<F>(description: &str, mut is_ready: F)
where
    F: FnMut() -> bool,
{
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if is_ready() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("{description}: timed out waiting for condition");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn bash_tool_executes_command_in_workspace() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let result = registry
        .execute(
            &ToolCall::new(
                "bash_1",
                "bash",
                BTreeMap::from([("command".to_string(), json!("echo hello"))]),
            ),
            &mut context,
        )
        .expect("bash tool");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert!(result.content.contains("\"exit_code\":0"));
    assert!(result.content.contains("hello"));
    assert!(!result.content.contains("\"command\""));
}

#[test]
fn bash_tools_reject_schema_invalid_argument_types() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    let cases = [
        (
            "bash",
            BTreeMap::from([
                ("command".to_string(), json!("printf no-run")),
                ("exec_dir".to_string(), json!(456)),
            ]),
            "/exec_dir",
        ),
        (
            "bash",
            BTreeMap::from([
                ("command".to_string(), json!("printf no-run")),
                ("stdin".to_string(), json!(123)),
            ]),
            "/stdin",
        ),
        (
            "bash",
            BTreeMap::from([
                ("command".to_string(), json!("printf no-run")),
                ("run_in_background".to_string(), json!("false")),
            ]),
            "/run_in_background",
        ),
        (
            "bash",
            BTreeMap::from([
                ("command".to_string(), json!("printf no-run")),
                ("timeout".to_string(), json!("1")),
            ]),
            "/timeout",
        ),
        (
            "check_background_command",
            BTreeMap::from([("session_id".to_string(), json!(123))]),
            "/session_id",
        ),
    ];

    for (tool_name, arguments, instance_path) in cases {
        let result = registry
            .execute(
                &ToolCall::new(format!("{tool_name}_invalid"), tool_name, arguments),
                &mut context,
            )
            .expect("tool validation");
        let payload: Value = serde_json::from_str(&result.content).expect("payload");
        assert_eq!(result.status, ToolResultStatus::Error);
        assert_eq!(result.error_code.as_deref(), Some("invalid_tool_arguments"));
        assert_eq!(payload["issues"][0]["instance_path"], instance_path);
        assert_eq!(payload["issues"][0]["rule"], "type");
    }
}

#[test]
fn bash_tool_blocks_dangerous_command() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let result = registry
        .execute(
            &ToolCall::new(
                "bash_1",
                "bash",
                BTreeMap::from([("command".to_string(), json!("rm -rf /"))]),
            ),
            &mut context,
        )
        .expect("bash tool");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("dangerous_command"));
}

#[test]
fn background_command_lifecycle_can_be_polled() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let start = registry
        .execute(
            &ToolCall::new(
                "bash_bg_1",
                "bash",
                BTreeMap::from([
                    (
                        "command".to_string(),
                        json!("printf start; sleep 0.2; printf done"),
                    ),
                    ("run_in_background".to_string(), json!(true)),
                    ("timeout".to_string(), json!(5)),
                ]),
            ),
            &mut context,
        )
        .expect("bash background start");

    assert_eq!(start.status, ToolResultStatus::Running);
    let start_payload: Value = serde_json::from_str(&start.content).expect("start payload");
    let session_id = start_payload["session_id"]
        .as_str()
        .expect("session_id")
        .to_string();
    assert_eq!(start_payload["status"], "running");
    assert!(start_payload.get("command").is_none());

    let final_payload = wait_for_background_payload("background command finished", || {
        let probe = registry
            .execute(
                &ToolCall::new(
                    "bash_bg_check_1",
                    "check_background_command",
                    BTreeMap::from([("session_id".to_string(), json!(session_id))]),
                ),
                &mut context,
            )
            .expect("check background command");
        let payload: Value = serde_json::from_str(&probe.content).expect("probe payload");
        if probe.status != ToolResultStatus::Running {
            assert_eq!(probe.status, ToolResultStatus::Success);
            assert_eq!(probe.metadata["status"], json!("completed"));
            assert_eq!(probe.metadata["exit_code"], json!(0));
        }
        payload
    });
    assert_eq!(final_payload["status"], "completed");
    assert_eq!(final_payload["exit_code"], 0);
    assert!(final_payload["command"]
        .as_str()
        .expect("command")
        .contains("printf start"));
    assert_eq!(final_payload["output"], "startdone");
}

#[test]
fn background_command_listener_receives_terminal_event() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let start = registry
        .execute(
            &ToolCall::new(
                "bash_bg_listener",
                "bash",
                BTreeMap::from([
                    ("command".to_string(), json!("printf listen; sleep 0.1")),
                    ("run_in_background".to_string(), json!(true)),
                    ("timeout".to_string(), json!(5)),
                ]),
            ),
            &mut context,
        )
        .expect("bash background start");
    let start_payload: Value = serde_json::from_str(&start.content).expect("start payload");
    let session_id = start_payload["session_id"]
        .as_str()
        .expect("session_id")
        .to_string();
    let events = Arc::new(Mutex::new(Vec::<Value>::new()));
    let sink = events.clone();
    let subscription = background_session_manager().subscribe(
        &session_id,
        Arc::new(move |payload| {
            sink.lock().expect("events").push(payload.clone());
        }),
    );

    wait_until(
        "background command listener receives terminal event",
        || {
            let probe = registry
                .execute(
                    &ToolCall::new(
                        "bash_bg_check_listener",
                        "check_background_command",
                        BTreeMap::from([("session_id".to_string(), json!(session_id))]),
                    ),
                    &mut context,
                )
                .expect("check background command");
            probe.status != ToolResultStatus::Running
        },
    );

    let events = events.lock().expect("events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["status"], "completed");
    assert_eq!(events[0]["output"], "listen");
    drop(subscription);
}

#[test]
fn background_command_listener_is_notified_without_polling() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let start = registry
        .execute(
            &ToolCall::new(
                "bash_bg_watch",
                "bash",
                BTreeMap::from([
                    ("command".to_string(), json!("printf watched")),
                    ("run_in_background".to_string(), json!(true)),
                    ("timeout".to_string(), json!(5)),
                ]),
            ),
            &mut context,
        )
        .expect("bash background start");
    let start_payload: Value = serde_json::from_str(&start.content).expect("start payload");
    let session_id = start_payload["session_id"]
        .as_str()
        .expect("session_id")
        .to_string();
    let events = Arc::new(Mutex::new(Vec::<Value>::new()));
    let sink = events.clone();
    let _subscription = background_session_manager().subscribe(
        &session_id,
        Arc::new(move |payload| {
            sink.lock().expect("events").push(payload.clone());
        }),
    );

    wait_until("background command listener is notified", || {
        !events.lock().expect("events").is_empty()
    });

    let events = events.lock().expect("events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["status"], "completed");
    assert_eq!(events[0]["output"], "watched");
}

#[test]
fn background_session_manager_can_start_process() {
    let workspace = tempfile::tempdir().expect("workspace");
    let session_id = background_session_manager()
        .start(
            "printf \"$VV_AGENT_BG_ENV\"",
            workspace.path(),
            5,
            BackgroundSessionStartOptions {
                shell: Some("bash".to_string()),
                env: Some(BTreeMap::from([(
                    "VV_AGENT_BG_ENV".to_string(),
                    "from-manager-start".to_string(),
                )])),
                ..Default::default()
            },
        )
        .expect("background session start");

    assert!(session_id.starts_with("bg_"));

    let final_payload = wait_for_background_payload("background manager task finished", || {
        background_session_manager().check(&session_id)
    });
    assert_eq!(final_payload["status"], "completed");
    assert_eq!(final_payload["exit_code"], 0);
    assert_eq!(final_payload["output"], "from-manager-start");
    assert!(final_payload["command"]
        .as_str()
        .expect("command")
        .contains("VV_AGENT_BG_ENV"));
}

#[test]
fn background_session_snapshot_keeps_null_shell() {
    let workspace = tempfile::tempdir().expect("workspace");
    let command = vec![
        "bash".to_string(),
        "-lc".to_string(),
        "printf null-shell".to_string(),
    ];
    let started = start_captured_process(&command, workspace.path(), None).expect("start process");
    let session_id = background_session_manager().adopt_running_process(
        "printf null-shell",
        workspace.path(),
        5,
        started.child,
        started.output_path,
        None,
    );

    let final_payload = wait_for_background_payload("background manager task finished", || {
        let payload = background_session_manager().check(&session_id);
        if payload["status"] == "running" {
            assert_eq!(payload.get("shell"), Some(&Value::Null));
        }
        payload
    });

    assert_eq!(final_payload["status"], "completed");
    assert_eq!(final_payload["output"], "null-shell");
    assert_eq!(final_payload.get("shell"), Some(&Value::Null));
}

#[test]
fn background_session_manager_can_adopt_running_process_with_started_at() {
    let workspace = tempfile::tempdir().expect("workspace");
    let command = vec![
        "bash".to_string(),
        "-lc".to_string(),
        "sleep 0.4; printf adopt-started-at".to_string(),
    ];
    let started = start_captured_process(&command, workspace.path(), None).expect("start process");
    let session_id = background_session_manager().adopt_running_process_with_options(
        BackgroundSessionAdoptOptions::new(
            "sleep 0.4; printf adopt-started-at",
            workspace.path(),
            5,
            started.child,
            started.output_path,
        )
        .with_shell("bash")
        .with_started_at(Instant::now() - Duration::from_secs(2)),
    );

    let payload = background_session_manager().check(&session_id);

    assert_eq!(payload["status"], "running");
    assert_eq!(payload["session_id"], session_id);
    assert_eq!(payload["command"], "sleep 0.4; printf adopt-started-at");
    assert_eq!(payload["shell"], "bash");
    assert!(
        payload["elapsed_seconds"]
            .as_f64()
            .expect("elapsed seconds")
            >= 1.5
    );
}

#[test]
fn background_session_timeout_kills_process_and_preserves_output() {
    let workspace = tempfile::tempdir().expect("workspace");
    let command = vec![
        "bash".to_string(),
        "-lc".to_string(),
        "printf background-partial; sleep 5".to_string(),
    ];
    let started = start_captured_process(&command, workspace.path(), None).expect("start process");
    let output_path = started.output_path.clone();
    wait_until("background timeout command emits partial output", || {
        read_captured_output(&output_path, 100).contains("background-partial")
    });
    assert!(
        read_captured_output(&output_path, 100).contains("background-partial"),
        "test setup should wait until the background process has emitted partial output"
    );
    let session_id = background_session_manager().adopt_running_process_with_options(
        BackgroundSessionAdoptOptions::new(
            "printf background-partial; sleep 5",
            workspace.path(),
            1,
            started.child,
            started.output_path,
        )
        .with_shell("bash")
        .with_started_at(Instant::now() - Duration::from_secs(2)),
    );

    let payload = background_session_manager().check(&session_id);

    assert_eq!(payload["status"], "timeout");
    assert_eq!(payload["session_id"], session_id);
    assert_eq!(payload["shell"], "bash");
    assert!(payload["output"]
        .as_str()
        .expect("output")
        .contains("background-partial"));
    assert_ne!(
        payload["exit_code"].as_i64().expect("exit_code"),
        0,
        "timed-out background sessions should report a non-zero exit code"
    );

    let second_check = background_session_manager().check(&session_id);
    assert_eq!(second_check["status"], "timeout");
    assert_eq!(second_check["output"], payload["output"]);
}

#[test]
fn foreground_timeout_moves_command_to_background() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let result = registry
        .execute(
            &ToolCall::new(
                "bash_timeout_1",
                "bash",
                BTreeMap::from([
                    ("command".to_string(), json!("printf partial; sleep 2")),
                    ("timeout".to_string(), json!(1)),
                ]),
            ),
            &mut context,
        )
        .expect("bash timeout");

    assert_eq!(result.status, ToolResultStatus::Running);
    let payload: Value = serde_json::from_str(&result.content).expect("timeout payload");
    assert_eq!(payload["status"], "running");
    assert_eq!(payload["transitioned_to_background"], true);
    assert!(payload["session_id"].as_str().is_some());
    assert!(payload["message"]
        .as_str()
        .expect("message")
        .contains("check_background_command"));
    assert!(payload["output"]
        .as_str()
        .expect("output")
        .contains("partial"));
}

#[test]
fn bash_tool_passes_stdin_to_command() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let result = registry
        .execute(
            &ToolCall::new(
                "bash_stdin_1",
                "bash",
                BTreeMap::from([
                    ("command".to_string(), json!("cat")),
                    ("stdin".to_string(), json!("hello from stdin\n")),
                ]),
            ),
            &mut context,
        )
        .expect("bash stdin");

    assert_eq!(result.status, ToolResultStatus::Success);
    let payload: Value = serde_json::from_str(&result.content).expect("stdin payload");
    assert_eq!(payload["exit_code"], 0);
    assert_eq!(payload["output"], "hello from stdin\n");
}

#[test]
fn captured_process_output_uses_replacement_decoding() {
    let workspace = tempfile::tempdir().expect("workspace");
    let output_path = workspace.path().join("invalid-output.log");
    std::fs::write(&output_path, b"ok\xffdone").expect("invalid utf8 output");

    let output = read_captured_output(&output_path, 20);

    assert_eq!(output, "ok\u{fffd}done");
}

#[test]
fn bash_tool_uses_configured_shell_from_metadata() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    context.metadata.insert(
        "bash_shell".to_string(),
        json!("definitely-missing-vv-agent-shell"),
    );

    let result = registry
        .execute(
            &ToolCall::new(
                "bash_missing_shell",
                "bash",
                BTreeMap::from([("command".to_string(), json!("echo should-not-run"))]),
            ),
            &mut context,
        )
        .expect("bash configured shell");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("command_failed"));
    assert!(result.content.contains("definitely-missing-vv-agent-shell"));
}

#[test]
fn bash_tool_uses_environment_from_metadata() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    context.metadata.insert(
        "bash_env".to_string(),
        json!({"VV_AGENT_TEST_ENV": "from-metadata"}),
    );

    let result = registry
        .execute(
            &ToolCall::new(
                "bash_env",
                "bash",
                BTreeMap::from([(
                    "command".to_string(),
                    json!("printf \"$VV_AGENT_TEST_ENV\""),
                )]),
            ),
            &mut context,
        )
        .expect("bash env");

    assert_eq!(result.status, ToolResultStatus::Success);
    let payload: Value = serde_json::from_str(&result.content).expect("env payload");
    assert_eq!(payload["output"], "from-metadata");
}

#[test]
fn bash_tool_rejects_invalid_environment_metadata() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    context
        .metadata
        .insert("bash_env".to_string(), json!("not-an-object"));

    let result = registry
        .execute(
            &ToolCall::new(
                "bash_bad_env",
                "bash",
                BTreeMap::from([("command".to_string(), json!("echo should-not-run"))]),
            ),
            &mut context,
        )
        .expect("bash env");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("invalid_shell_config"));
    assert!(result.content.contains("bash_env"));
}

#[test]
fn bash_tool_rejects_exec_dir_outside_workspace_by_default() {
    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let result = registry
        .execute(
            &ToolCall::new(
                "bash_escape",
                "bash",
                BTreeMap::from([
                    ("command".to_string(), json!("pwd")),
                    ("exec_dir".to_string(), json!(outside.path())),
                ]),
            ),
            &mut context,
        )
        .expect("bash tool");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("path_escapes_workspace"));
}

#[test]
fn bash_tool_allows_absolute_exec_dir_when_enabled() {
    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    context
        .metadata
        .insert("allow_outside_workspace_paths".to_string(), json!(true));

    let result = registry
        .execute(
            &ToolCall::new(
                "bash_absolute_exec_dir",
                "bash",
                BTreeMap::from([
                    ("command".to_string(), json!("printf outside")),
                    ("exec_dir".to_string(), json!(outside.path())),
                ]),
            ),
            &mut context,
        )
        .expect("bash tool");

    assert_eq!(result.status, ToolResultStatus::Success);
    let payload: Value = serde_json::from_str(&result.content).expect("bash payload");
    assert_eq!(payload["cwd"], json!(outside.path().to_string_lossy()));
    assert_eq!(payload["output"], json!("outside"));
}
