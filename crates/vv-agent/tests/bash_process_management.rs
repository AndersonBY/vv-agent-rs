use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use vv_agent::checkpoint::tool_result_digest;
use vv_agent::{
    build_default_registry, Agent, AgentStatus, CapabilityRef, CheckpointConfig, CheckpointStore,
    LLMResponse, ModelRef, NoToolPolicy, RunConfig, Runner, ScriptStep, ScriptedModelProvider,
    SqliteCheckpointStore, StaticTool, ToolCall, ToolContext, ToolDirective, ToolExecutionResult,
    ToolPolicy, ToolRegistry, ToolResultStatus,
};

const FIXTURE: &str = include_str!("fixtures/parity/bash_process_management.json");

fn fixture() -> Value {
    serde_json::from_str(FIXTURE).expect("shared C21 process contract")
}

fn args(value: Value) -> BTreeMap<String, Value> {
    serde_json::from_value(value).expect("tool arguments")
}

fn context(root: &Path, task_id: &str) -> ToolContext {
    let mut context = ToolContext::new(root);
    context.task_id = task_id.to_string();
    context
}

fn call(context: &mut ToolContext, name: &str, arguments: Value) -> ToolExecutionResult {
    context.tool_call_id = format!("call_{name}");
    build_default_registry()
        .execute(
            &ToolCall::new(&context.tool_call_id, name, args(arguments)),
            context,
        )
        .expect("real built-in producer")
}

fn runner_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    for executor in build_default_registry().executors() {
        if matches!(
            executor.name(),
            "bash" | "check_background_command" | "stop_background_command"
        ) {
            registry
                .register_executor(executor)
                .expect("real default executor");
        }
    }
    registry
}

fn child_arguments(mode: &str, yield_time_ms: u64) -> Value {
    json!({
        "command": format!("\"{}\" --exact managed_child --nocapture", std::env::current_exe().expect("test executable").display()),
        "stdin": json!({"mode": mode}).to_string(),
        "yield_time_ms": yield_time_ms,
        "timeout_seconds": 10,
    })
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "local child did not reach expected state"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn assert_running(result: &ToolExecutionResult) -> String {
    assert_eq!(result.status, ToolResultStatus::Success, "{result:?}");
    assert_eq!(result.directive, ToolDirective::Continue);
    assert_eq!(result.metadata["status"], "running");
    for field in fixture()["running_receipt"]["forbidden_metadata"]
        .as_array()
        .unwrap()
    {
        assert!(!result.metadata.contains_key(field.as_str().unwrap()));
    }
    let payload: Value = serde_json::from_str(&result.content).expect("management receipt");
    assert_eq!(payload["status"], "running");
    assert_eq!(payload["session_id"], result.metadata["session_id"]);
    payload["session_id"].as_str().unwrap().to_string()
}

struct StopOnDrop {
    root: PathBuf,
    task_id: String,
    session_id: String,
}

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        let _ = call(
            &mut context(&self.root, &self.task_id),
            "stop_background_command",
            json!({"session_id": self.session_id}),
        );
    }
}

fn guard(context: &ToolContext, session_id: &str) -> StopOnDrop {
    StopOnDrop {
        root: context.workspace.clone(),
        task_id: context.task_id.clone(),
        session_id: session_id.to_string(),
    }
}

// Re-execute this test binary as an actual local child. A normal test-suite run
// does not select this helper with --exact and returns immediately.
#[test]
fn managed_child() {
    if !std::env::args().any(|value| value == "--exact") {
        return;
    }
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .expect("child input");
    let config: Value = serde_json::from_str(&input).expect("child configuration");
    match config["mode"].as_str().expect("mode") {
        #[cfg(target_os = "linux")]
        "closed-stdio" => {
            use vv_agent::runtime::processes::{
                kill_process_tree, remove_captured_output, start_captured_process,
            };
            unsafe {
                libc::close(0);
                libc::close(1);
                libc::close(2);
            }
            let mut captured = start_captured_process(
                &[
                    "sh".to_string(),
                    "-c".to_string(),
                    "printf CLOSED_STDIO_OK".to_string(),
                ],
                Path::new("."),
                None,
            )
            .unwrap();
            let status = captured.child.wait().unwrap();
            let confirmed = kill_process_tree(&mut captured.child);
            let output = std::fs::read_to_string(&captured.output_path).unwrap();
            remove_captured_output(&captured.output_path);
            std::fs::write(
                "closed-stdio-result.json",
                json!({"code": status.code(), "confirmed": confirmed, "output": output})
                    .to_string(),
            )
            .unwrap();
            std::process::exit(0);
        }
        "http" => {
            let server = TcpListener::bind(("127.0.0.1", 0)).expect("loopback server");
            println!("头{}尾", "x".repeat(15000));
            std::io::stdout().flush().unwrap();
            std::fs::write("port", server.local_addr().unwrap().port().to_string()).unwrap();
            for stream in server.incoming() {
                let mut stream = stream.expect("loopback request");
                let mut request = [0; 2048];
                let bytes = stream.read(&mut request).unwrap();
                println!(
                    "日志{}",
                    String::from_utf8_lossy(&request[..bytes])
                        .lines()
                        .next()
                        .unwrap_or_default()
                );
                std::io::stdout().flush().unwrap();
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\nConnection: close\r\n\r\nLOCAL_HTTP_OK").unwrap();
            }
        }
        "output" => {
            println!("头{}尾", "A".repeat(13000));
            std::io::stdout().flush().unwrap();
            std::fs::write("ready", "").unwrap();
            while !Path::new("more").exists() {
                thread::sleep(Duration::from_millis(10));
            }
            println!("追加😀尾部");
            std::io::stdout().flush().unwrap();
            std::fs::write("updated", "").unwrap();
            while !Path::new("finish").exists() {
                thread::sleep(Duration::from_millis(10));
            }
        }
        "delay" => {
            println!("ready");
            std::io::stdout().flush().unwrap();
            thread::sleep(Duration::from_secs(10));
        }
        #[cfg(unix)]
        "orphan" => {
            std::fs::write("parent-pid", std::process::id().to_string()).unwrap();
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "managed_child", "--nocapture"])
                .stdin(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(br#"{"mode":"orphan-child"}"#)
                .unwrap();
            wait_until(|| Path::new("child-ready").exists());
            // This helper intentionally exits before its inherited-group child.
            std::process::exit(0);
        }
        #[cfg(unix)]
        "orphan-child" => {
            unsafe {
                libc::signal(libc::SIGTERM, libc::SIG_IGN);
            }
            std::fs::write("child-ready", std::process::id().to_string()).unwrap();
            thread::sleep(Duration::from_secs(20));
        }
        #[cfg(target_os = "linux")]
        "detached" | "detached-double" | "detached-signal" | "detached-intermediate" => {
            use std::os::unix::process::CommandExt;
            let mode = config["mode"].as_str().unwrap();
            if mode != "detached-intermediate" {
                std::fs::write("parent-pid", std::process::id().to_string()).unwrap();
            }
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", "managed_child", "--nocapture"])
                .stdin(std::process::Stdio::piped());
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut child = command.spawn().unwrap();
            let child_mode = if matches!(mode, "detached-double" | "detached-signal") {
                "detached-intermediate"
            } else {
                "detached-child"
            };
            child
                .stdin
                .take()
                .unwrap()
                .write_all(json!({"mode": child_mode}).to_string().as_bytes())
                .unwrap();
            wait_until(|| Path::new("child-ready").exists());
            if mode == "detached-intermediate" {
                return;
            }
            if mode == "detached-signal" {
                unsafe {
                    libc::raise(libc::SIGTERM);
                }
            }
            std::process::exit(7);
        }
        #[cfg(target_os = "linux")]
        "detached-child" => {
            unsafe {
                libc::signal(libc::SIGTERM, libc::SIG_IGN);
            }
            println!("DETACHED_READY{}", "x".repeat(13000));
            std::io::stdout().flush().unwrap();
            std::fs::write("child-ready", std::process::id().to_string()).unwrap();
            while !Path::new("finish").exists() {
                thread::sleep(Duration::from_millis(10));
            }
            println!("DETACHED_DONE");
        }
        mode => panic!("unknown child mode: {mode}"),
    }
}

#[test]
fn managed_unread_child() {
    if std::env::args().any(|value| value == "--exact") {
        thread::sleep(Duration::from_secs(10));
    }
}

#[test]
fn shared_contract_arguments_are_rejected_by_real_registries() {
    let workspace = tempfile::tempdir().unwrap();
    let mut context = context(workspace.path(), "owner");
    let fixture = fixture();
    let invalid = fixture["invalid_bash_arguments"].as_array().unwrap();
    for case in invalid {
        let result = call(&mut context, "bash", case["arguments"].clone());
        assert_eq!(result.status, ToolResultStatus::Error, "{}", case["name"]);
        assert_eq!(
            result.error_code.as_deref(),
            Some("invalid_tool_arguments"),
            "{}",
            case["name"]
        );
    }
    for tool in ["check_background_command", "stop_background_command"] {
        for case in fixture["invalid_management_arguments"].as_array().unwrap() {
            let result = call(&mut context, tool, case["arguments"].clone());
            assert_eq!(result.status, ToolResultStatus::Error);
            assert_eq!(
                result.error_code.as_deref(),
                Some("invalid_tool_arguments"),
                "{tool}/{}",
                case["name"]
            );
        }
    }
}

fn checkpoint_receipt(
    store: &SqliteCheckpointStore,
    key: &str,
    call_id: &str,
) -> ToolExecutionResult {
    store
        .load_checkpoint(key)
        .unwrap()
        .unwrap()
        .cycles
        .into_iter()
        .flat_map(|cycle| cycle.tool_results)
        .find(|result| result.tool_call_id == call_id)
        .expect("durable committed tool receipt")
}

fn request_http(port: u16) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .write_all(b"GET /before-query HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

async fn runner_management_case(yield_time_ms: u64) {
    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path().to_path_buf();
    let store = Arc::new(SqliteCheckpointStore::new(root.join("checkpoint.sqlite")).unwrap());
    let key = "runner-bash";
    let seen = Arc::new(Mutex::new(Vec::new()));
    let receipts = Arc::new(Mutex::new(BTreeMap::new()));
    let session_id = Arc::new(Mutex::new(String::new()));
    let cleanup = Arc::new(Mutex::new(None::<StopOnDrop>));
    let mut steps = vec![ScriptStep::response(LLMResponse::with_tool_calls(
        "",
        vec![
            ToolCall::new(
                "start",
                "bash",
                args(child_arguments("http", yield_time_ms)),
            ),
            ToolCall::new("batch", "batch_marker", BTreeMap::new()),
        ],
    ))];
    {
        let (root, store, seen, receipts, session_id, cleanup) = (
            root.clone(),
            store.clone(),
            seen.clone(),
            receipts.clone(),
            session_id.clone(),
            cleanup.clone(),
        );
        steps.push(ScriptStep::callback(move |request| {
            seen.lock().unwrap().push("model-after-start");
            assert!(request
                .messages
                .iter()
                .any(|message| message.tool_call_id.as_deref() == Some("start")));
            let receipt = checkpoint_receipt(&store, key, "start");
            let id = assert_running(&receipt);
            let checkpoint = store.load_checkpoint(key).unwrap().unwrap();
            *cleanup.lock().unwrap() = Some(StopOnDrop {
                root: root.clone(),
                task_id: checkpoint.task_id,
                session_id: id.clone(),
            });
            assert_eq!(
                checkpoint_receipt(&store, key, "batch").status,
                ToolResultStatus::Success
            );
            receipts.lock().unwrap().insert(
                "start",
                (receipt.clone(), tool_result_digest(&receipt).unwrap()),
            );
            *session_id.lock().unwrap() = id.clone();
            wait_until(|| root.join("port").exists());
            let port = std::fs::read_to_string(root.join("port"))
                .unwrap()
                .parse()
                .unwrap();
            assert!(request_http(port).ends_with("LOCAL_HTTP_OK"));
            Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::new(
                    "query",
                    "check_background_command",
                    args(json!({"session_id": id})),
                )],
            ))
        }));
    }
    {
        let (store, seen, receipts, session_id) = (
            store.clone(),
            seen.clone(),
            receipts.clone(),
            session_id.clone(),
        );
        steps.push(ScriptStep::callback(move |_| {
            seen.lock().unwrap().push("model-after-query");
            let receipt = checkpoint_receipt(&store, key, "query");
            assert_running(&receipt);
            assert!(receipt.truncated);
            assert!(receipt.content.contains("日志GET /before-query"));
            receipts.lock().unwrap().insert(
                "query",
                (receipt.clone(), tool_result_digest(&receipt).unwrap()),
            );
            Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::new(
                    "stop",
                    "stop_background_command",
                    args(json!({"session_id": *session_id.lock().unwrap()})),
                )],
            ))
        }));
    }
    {
        let (store, seen) = (store.clone(), seen.clone());
        steps.push(ScriptStep::callback(move |_| {
            seen.lock().unwrap().push("model-after-stop");
            let receipt = checkpoint_receipt(&store, key, "stop");
            assert_eq!(receipt.metadata["status"], "stopped");
            assert_ne!(receipt.metadata["exit_code"], 0);
            assert_eq!(receipt.status, ToolResultStatus::Error);
            Ok(LLMResponse::new("NEXT_MODEL_CYCLE_OK"))
        }));
    }
    let seen_for_tool = seen.clone();
    let marker = StaticTool::new(
        "batch_marker",
        "Record the mixed tool batch.",
        json!({"type": "object", "properties": {}, "additionalProperties": false}),
        Arc::new(move |_, _| {
            seen_for_tool.lock().unwrap().push("mixed-tool");
            ToolExecutionResult::success("", "MIXED_TOOL_OK")
        }),
    );
    let agent = Agent::builder("bash-runner")
        .instructions("Use the local command tools.")
        .model(ModelRef::named("local"))
        .tool(marker)
        .tool_policy(ToolPolicy::default().allow_only([
            "bash",
            "check_background_command",
            "stop_background_command",
            "batch_marker",
        ]))
        .build()
        .unwrap();
    let runner = Runner::builder()
        .workspace(&root)
        .model_provider(ScriptedModelProvider::from_steps(
            "scripted", "local", steps,
        ))
        .build()
        .unwrap();
    let mut checkpoint = CheckpointConfig::new(store.clone());
    checkpoint.key = Some(key.to_string());
    checkpoint.capability_refs.insert(
        "workspace".to_string(),
        CapabilityRef {
            id: "test.local-workspace".to_string(),
            version: "1".to_string(),
        },
    );
    checkpoint.capability_refs.insert(
        "tool_registry_factory".to_string(),
        CapabilityRef {
            id: "test.real-bash-registry".to_string(),
            version: "21".to_string(),
        },
    );
    let result = runner
        .run_with_config(
            &agent,
            "Start, inspect and stop the local HTTP server.",
            RunConfig::builder()
                .max_cycles(5)
                .no_tool_policy(NoToolPolicy::Finish)
                .tool_registry_factory(runner_registry)
                .checkpoint_config(checkpoint)
                .build(),
        )
        .await
        .unwrap();
    assert_eq!(
        result.status(),
        AgentStatus::Completed,
        "{:?}",
        result.result()
    );
    assert_eq!(result.final_output(), Some("NEXT_MODEL_CYCLE_OK"));
    assert_eq!(
        *seen.lock().unwrap(),
        [
            "mixed-tool",
            "model-after-start",
            "model-after-query",
            "model-after-stop"
        ]
    );
    for (call_id, (original, digest)) in receipts.lock().unwrap().iter() {
        let current = checkpoint_receipt(&store, key, call_id);
        assert_eq!(current.to_dict(), original.to_dict());
        assert_eq!(tool_result_digest(&current).unwrap(), *digest);
    }
    let port: u16 = std::fs::read_to_string(root.join("port"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(TcpStream::connect(("127.0.0.1", port)).is_err());
    cleanup.lock().unwrap().take();
}

#[tokio::test]
async fn real_runner_checkpoint_continues_after_immediate_handle_query_and_stop() {
    runner_management_case(
        fixture()["launch_cases"][0]["yield_time_ms"]
            .as_u64()
            .unwrap(),
    )
    .await;
}

#[tokio::test]
async fn real_runner_checkpoint_continues_after_yield_expiry_query_and_stop() {
    runner_management_case(
        fixture()["launch_cases"][1]["yield_time_ms"]
            .as_u64()
            .unwrap(),
    )
    .await;
}

#[test]
fn execution_deadline_uses_original_process_start_and_needs_no_query() {
    let workspace = tempfile::tempdir().unwrap();
    let mut context = context(workspace.path(), "owner");
    let mut arguments = child_arguments("delay", 800);
    arguments["timeout_seconds"] = json!(1);
    let started = Instant::now();
    let id = assert_running(&call(&mut context, "bash", arguments));
    let _cleanup = guard(&context, &id);
    assert!(started.elapsed() >= Duration::from_millis(800));
    let _ = call(
        &mut context,
        "check_background_command",
        json!({"session_id": id}),
    );
    thread::sleep(Duration::from_millis(700));
    let result = call(
        &mut context,
        "check_background_command",
        json!({"session_id": id}),
    );
    assert_eq!(result.metadata["status"], "timeout", "{result:?}");
    assert_eq!(result.status, ToolResultStatus::Error);
    assert_ne!(result.metadata["exit_code"], 0);
    assert!(result.content.contains("ready"));
}

#[test]
fn foreign_task_and_workspace_cannot_observe_or_stop_a_session() {
    let workspace = tempfile::tempdir().unwrap();
    let other_workspace = tempfile::tempdir().unwrap();
    let mut owner = context(workspace.path(), "owner");
    let id = assert_running(&call(&mut owner, "bash", child_arguments("http", 0)));
    let _cleanup = guard(&owner, &id);
    wait_until(|| workspace.path().join("port").exists());
    let port = std::fs::read_to_string(workspace.path().join("port"))
        .unwrap()
        .parse()
        .unwrap();
    for mut foreign in [
        context(workspace.path(), "foreign"),
        context(other_workspace.path(), "owner"),
    ] {
        for tool in ["check_background_command", "stop_background_command"] {
            let result = call(&mut foreign, tool, json!({"session_id": id}));
            assert_eq!(result.status, ToolResultStatus::Error);
            assert_eq!(
                result.error_code.as_deref(),
                Some("background_session_forbidden")
            );
            assert!(!result.content.contains("日志"));
            assert!(result.artifact.is_none());
            assert!(request_http(port).ends_with("LOCAL_HTTP_OK"));
        }
    }
}

#[test]
fn missing_local_manager_records_do_not_claim_exit() {
    let workspace = tempfile::tempdir().unwrap();
    let mut context = context(workspace.path(), "owner");
    for tool in ["check_background_command", "stop_background_command"] {
        let result = call(
            &mut context,
            tool,
            json!({"session_id": "lost-after-restart"}),
        );
        assert_eq!(result.status, ToolResultStatus::Error);
        assert_eq!(result.metadata["status"], "missing");
        assert!(!result.metadata.contains_key("exit_code"));
        assert!(serde_json::from_str::<Value>(&result.content)
            .unwrap()
            .get("exit_code")
            .is_none());
    }
}

#[test]
fn running_unicode_tail_and_complete_artifacts_remain_immutable() {
    let workspace = tempfile::tempdir().unwrap();
    let mut context = context(workspace.path(), "owner");
    let mut arguments = child_arguments("output", 0);
    arguments.as_object_mut().unwrap().remove("timeout_seconds");
    let id = assert_running(&call(&mut context, "bash", arguments));
    let _cleanup = guard(&context, &id);
    wait_until(|| workspace.path().join("ready").exists());
    let first = call(
        &mut context,
        "check_background_command",
        json!({"session_id": id}),
    );
    assert_running(&first);
    let first_artifact = first.artifact.as_ref().unwrap();
    let backend = context.effective_workspace_backend();
    let full_first = backend.read_text(&first_artifact.path).unwrap();
    assert_eq!(first.visible_bytes, Some(first.content.len() as u64));
    assert!(first.original_bytes >= first.visible_bytes);
    assert_eq!(
        first_artifact.sha256,
        format!("{:x}", Sha256::digest(full_first.as_bytes()))
    );
    std::fs::write(workspace.path().join("more"), "").unwrap();
    wait_until(|| workspace.path().join("updated").exists());
    let second = call(
        &mut context,
        "check_background_command",
        json!({"session_id": id}),
    );
    assert_running(&second);
    assert!(
        serde_json::from_str::<Value>(&second.content).unwrap()["output"]
            .as_str()
            .unwrap()
            .ends_with("追加😀尾部\n")
    );
    assert_eq!(backend.read_text(&first_artifact.path).unwrap(), full_first);
    assert!(backend
        .read_text(&second.artifact.as_ref().unwrap().path)
        .unwrap()
        .ends_with("追加😀尾部\n"));
    std::fs::write(workspace.path().join("finish"), "").unwrap();
    let mut terminal = None;
    wait_until(|| {
        let result = call(
            &mut context,
            "check_background_command",
            json!({"session_id": id}),
        );
        if result.metadata["status"] == "completed" {
            terminal = Some(result);
            true
        } else {
            false
        }
    });
    let repeated = call(
        &mut context,
        "check_background_command",
        json!({"session_id": id}),
    );
    let terminal = terminal.unwrap();
    assert_eq!(terminal.artifact, repeated.artifact);
    assert_eq!(terminal.content, repeated.content);
}

#[test]
fn unread_stdin_does_not_block_initial_yield_or_timeout() {
    let workspace = tempfile::tempdir().unwrap();
    let mut context = context(workspace.path(), "owner");
    let started = Instant::now();
    let id = assert_running(&call(
        &mut context,
        "bash",
        json!({
            "command": format!("\"{}\" --exact managed_unread_child --nocapture", std::env::current_exe().unwrap().display()),
            "stdin": "x".repeat(2_000_000), "yield_time_ms": 0, "timeout_seconds": 1,
        }),
    ));
    let _cleanup = guard(&context, &id);
    assert!(started.elapsed() < Duration::from_millis(750));
    thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        call(
            &mut context,
            "check_background_command",
            json!({"session_id": id})
        )
        .metadata["status"],
        "timeout"
    );
}

#[test]
#[cfg(unix)]
fn stop_confirms_children_after_the_shell_parent_has_exited() {
    let workspace = tempfile::tempdir().unwrap();
    let mut context = context(workspace.path(), "owner");
    let id = assert_running(&call(&mut context, "bash", child_arguments("orphan", 200)));
    let _cleanup = guard(&context, &id);
    wait_until(|| workspace.path().join("child-ready").exists());
    let parent = std::fs::read_to_string(workspace.path().join("parent-pid")).unwrap();
    wait_until(|| process_exited(&parent));
    let result = call(
        &mut context,
        "stop_background_command",
        json!({"session_id": id}),
    );
    assert_eq!(result.metadata["status"], "stopped");
    assert_eq!(result.metadata["exit_code"], 0);
    let pid = std::fs::read_to_string(workspace.path().join("child-ready")).unwrap();
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        let state = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .next()
            .unwrap();
        assert!(matches!(state, "Z" | "X"));
    }
}

#[test]
#[cfg(target_os = "linux")]
fn detached_child_remains_managed_after_parent_exit() {
    detached_case("detached", "stop", 7);
}

#[test]
#[cfg(target_os = "linux")]
fn double_fork_retains_original_parent_signal_after_stop() {
    detached_case("detached-signal", "stop", -15);
}

#[test]
#[cfg(target_os = "linux")]
fn detached_double_fork_deadline_needs_no_query() {
    detached_case("detached-double", "timeout", 7);
}

#[test]
#[cfg(target_os = "linux")]
fn detached_double_fork_completion_keeps_last_output() {
    detached_case("detached-double", "complete", 7);
}

#[cfg(unix)]
fn process_exited(pid: &str) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |stat| {
        matches!(
            stat.rsplit_once(')').unwrap().1.split_whitespace().next(),
            Some("Z" | "X")
        )
    })
}

#[cfg(target_os = "linux")]
fn detached_case(mode: &str, operation: &str, exit_code: i64) {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let workspace = tempfile::tempdir().unwrap();
    let mut context = context(workspace.path(), "owner");
    let mut arguments = child_arguments(mode, 0);
    arguments["command"] = json!(format!("exec {}", arguments["command"].as_str().unwrap()));
    if operation == "timeout" {
        arguments["timeout_seconds"] = json!(1);
    }
    let started = call(&mut context, "bash", arguments);
    let id = started.metadata["session_id"].as_str().unwrap().to_string();
    let _cleanup = guard(&context, &id);
    wait_until(|| workspace.path().join("child-ready").exists());
    let pid: i32 = std::fs::read_to_string(workspace.path().join("child-ready"))
        .unwrap()
        .parse()
        .unwrap();
    let raw_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32 };
    assert!(
        raw_fd >= 0,
        "pidfd_open: {}",
        std::io::Error::last_os_error()
    );
    struct ChildCleanup(OwnedFd);
    impl Drop for ChildCleanup {
        fn drop(&mut self) {
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.0.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
        }
    }
    let _child_cleanup = ChildCleanup(unsafe { OwnedFd::from_raw_fd(raw_fd) });
    let parent = std::fs::read_to_string(workspace.path().join("parent-pid")).unwrap();
    wait_until(|| process_exited(&parent));
    let active = call(
        &mut context,
        "check_background_command",
        json!({"session_id": id}),
    );
    println!("DETACHED_OBSERVATION {:?}", active.metadata);
    assert_running(&active);
    assert!(active.content.contains("DETACHED_READY"));
    assert!(active.artifact.is_some());
    let stopped = if operation == "stop" {
        call(
            &mut context,
            "stop_background_command",
            json!({"session_id": id}),
        )
    } else {
        if operation == "complete" {
            std::fs::write(workspace.path().join("finish"), "").unwrap();
        } else {
            // The deadline must kill the detached tree without a polling query.
            thread::sleep(Duration::from_millis(1400));
        }
        let mut terminal = None;
        wait_until(|| {
            let result = call(
                &mut context,
                "check_background_command",
                json!({"session_id": id}),
            );
            if !matches!(
                result.metadata["status"].as_str(),
                Some("running" | "stopping" | "unknown")
            ) {
                terminal = Some(result);
                true
            } else {
                false
            }
        });
        terminal.unwrap()
    };
    assert_eq!(
        stopped.metadata["status"],
        match operation {
            "stop" => "stopped",
            "timeout" => "timeout",
            _ => "failed",
        }
    );
    assert_eq!(stopped.metadata["exit_code"], exit_code);
    assert!(process_exited(&pid.to_string()));
    let backend = context.effective_workspace_backend();
    let terminal_text = backend.read_text(&stopped.artifact.unwrap().path).unwrap();
    assert!(terminal_text.contains("DETACHED_READY"));
    if operation == "complete" {
        assert!(terminal_text.contains("DETACHED_DONE"));
    }
    assert!(!backend
        .read_text(&active.artifact.unwrap().path)
        .unwrap()
        .contains("DETACHED_DONE"));
}

#[test]
#[cfg(target_os = "linux")]
fn supervisor_does_not_adopt_unrelated_children_or_inherit_host_sockets() {
    use std::os::unix::process::ExitStatusExt;
    use vv_agent::runtime::processes::{
        kill_process_tree, remove_captured_output, start_captured_process, CapturedProcess,
    };
    fn subreaper_value() -> i32 {
        let mut value = 0;
        assert_eq!(
            unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut value, 0, 0, 0) },
            0
        );
        value
    }
    struct Cleanup(CapturedProcess);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            kill_process_tree(&mut self.0.child);
            remove_captured_output(&self.0.output_path);
        }
    }
    let before = subreaper_value();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let mut unrelated = std::process::Command::new("sh")
        .args(["-c", "sleep 0.1; exit 23"])
        .spawn()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut captured = Cleanup(
        start_captured_process(&["sleep".to_string(), "10".to_string()], root.path(), None)
            .unwrap(),
    );
    drop(listener);
    let _replacement = TcpListener::bind(address).unwrap();
    assert_eq!(subreaper_value(), before);
    assert_eq!(unrelated.wait().unwrap().code(), Some(23));
    assert!(captured.0.child.try_wait().unwrap().is_none());
    assert!(kill_process_tree(&mut captured.0.child));
    assert_eq!(
        captured.0.child.wait().unwrap().signal(),
        Some(libc::SIGTERM)
    );
}

#[test]
#[cfg(target_os = "linux")]
fn supervisor_control_survives_closed_application_stdio() {
    let root = tempfile::tempdir().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "managed_child", "--nocapture"])
        .current_dir(root.path())
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"mode":"closed-stdio"}"#)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("closed-stdio probe did not finish");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let result: Value = serde_json::from_str(
        &std::fs::read_to_string(root.path().join("closed-stdio-result.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        result,
        json!({"code": 0, "confirmed": true, "output": "CLOSED_STDIO_OK"})
    );
}
