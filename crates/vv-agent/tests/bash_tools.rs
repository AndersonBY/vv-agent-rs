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
    assert_eq!(result.content, "hello\n");
    assert_eq!(result.metadata["exit_code"], 0);
    assert_eq!(result.metadata["cwd"], ".");
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

    let deadline = Instant::now() + Duration::from_secs(10);
    let final_result = loop {
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
        if probe.status == ToolResultStatus::Running {
            assert!(Instant::now() < deadline, "background command timed out");
            thread::sleep(Duration::from_millis(50));
            continue;
        }
        break probe;
    };
    assert_eq!(final_result.status, ToolResultStatus::Success);
    assert_eq!(final_result.metadata["status"], json!("completed"));
    assert_eq!(final_result.metadata["exit_code"], json!(0));
    assert_eq!(final_result.content, "startdone");
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
    assert_eq!(result.metadata["exit_code"], 0);
    assert_eq!(result.content, "hello from stdin\n");
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
    assert_eq!(result.content, "from-metadata");
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
    assert_eq!(
        result.metadata["cwd"],
        json!(outside.path().to_string_lossy())
    );
    assert_eq!(result.content, "outside");
}

#[test]
fn foreground_bash_uses_exact_preview_boundary_and_persists_complete_artifact() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    context.task_id = "task-7".to_string();

    let exact = registry
        .execute(
            &ToolCall::new(
                "bash_exact",
                "bash",
                BTreeMap::from([("command".to_string(), json!("printf '%012000d' 0"))]),
            ),
            &mut context,
        )
        .expect("exact boundary");
    assert_eq!(exact.content.chars().count(), 12_000);
    assert!(!exact.truncated);
    assert!(exact.artifact.is_none());

    let truncated = registry
        .execute(
            &ToolCall::new(
                "bash_truncated",
                "bash",
                BTreeMap::from([(
                    "command".to_string(),
                    json!(concat!(
                        "printf '%*s' 6000 '' | tr ' ' A; ",
                        "printf '%*s' 48 '' | tr ' ' M; ",
                        "printf '%*s' 5953 '' | tr ' ' Z"
                    )),
                )]),
            ),
            &mut context,
        )
        .expect("truncated output");

    assert!(truncated.truncated);
    assert_eq!(truncated.content.chars().count(), 12_000);
    assert_eq!(
        truncated.content,
        format!(
            "{}\n... output omitted; full text in artifact ...\n{}",
            "A".repeat(6_000),
            "Z".repeat(5_953)
        )
    );
    assert_eq!(truncated.original_bytes, Some(12_001));
    assert_eq!(truncated.visible_bytes, Some(12_000));
    let artifact = truncated.artifact.expect("artifact");
    assert!(artifact.path.starts_with(".vv-agent/artifacts/task-7/"));
    assert_eq!(artifact.size_bytes, 12_001);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join(&artifact.path)).expect("artifact text"),
        format!(
            "{}{}{}",
            "A".repeat(6_000),
            "M".repeat(48),
            "Z".repeat(5_953)
        )
    );
}

#[test]
fn background_bash_reuses_terminal_artifact_across_polls() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    context.task_id = "background-task".to_string();
    let start = registry
        .execute(
            &ToolCall::new(
                "background-large",
                "bash",
                BTreeMap::from([
                    ("command".to_string(), json!("printf '%012001d' 0")),
                    ("run_in_background".to_string(), json!(true)),
                ]),
            ),
            &mut context,
        )
        .expect("background start");
    let session_id = serde_json::from_str::<Value>(&start.content).expect("start payload")
        ["session_id"]
        .as_str()
        .expect("session id")
        .to_string();

    let deadline = Instant::now() + Duration::from_secs(10);
    let first = loop {
        let result = registry
            .execute(
                &ToolCall::new(
                    "background-large-check",
                    "check_background_command",
                    BTreeMap::from([("session_id".to_string(), json!(session_id))]),
                ),
                &mut context,
            )
            .expect("background check");
        if result.status != ToolResultStatus::Running {
            break result;
        }
        assert!(Instant::now() < deadline, "background artifact timed out");
        thread::sleep(Duration::from_millis(50));
    };
    let first_artifact = first.artifact.clone().expect("first artifact");
    assert!(first.truncated);

    let second = registry
        .execute(
            &ToolCall::new(
                "background-large-check-again",
                "check_background_command",
                BTreeMap::from([("session_id".to_string(), json!(session_id))]),
            ),
            &mut context,
        )
        .expect("second background check");
    assert_eq!(second.artifact.as_ref(), Some(&first_artifact));
    assert_eq!(second.content, first.content);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join(first_artifact.path))
            .expect("background artifact")
            .chars()
            .count(),
        12_001
    );
}
