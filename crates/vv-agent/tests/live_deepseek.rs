use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use vv_agent::constants::{
    ASK_USER_TOOL_NAME, BASH_TOOL_NAME, CHECK_BACKGROUND_COMMAND_TOOL_NAME, READ_FILE_TOOL_NAME,
    WRITE_FILE_TOOL_NAME,
};
use vv_agent::{
    build_vv_llm_from_local_settings, AgentDefinition, AgentRuntime, AgentSDKClient,
    AgentSDKOptions, AgentStatus, AgentTask, NoToolPolicy,
};

#[test]
#[ignore = "live API call; run with VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_deepseek -- --ignored"]
fn live_deepseek_v4_pro_finishes_agent_task() {
    if !live_enabled() {
        eprintln!("Live tests are disabled. Set VV_AGENT_RUN_LIVE_TESTS=1 to run.");
        return;
    }

    let settings_path = live_settings_path();
    assert!(
        settings_path.exists(),
        "live settings file is missing: {}",
        settings_path.display()
    );

    let (llm, resolved) =
        build_vv_llm_from_local_settings(&settings_path, "deepseek", "deepseek-v4-pro", 90.0)
            .expect("build DeepSeek vv-llm client");
    assert_eq!(resolved.backend, "deepseek");
    assert_eq!(resolved.requested_model, "deepseek-v4-pro");

    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new(
        "live_deepseek_v4_pro",
        resolved.model_id.clone(),
        "You are testing an agent runtime. You must call the task_finish tool. \
         Set the task_finish message to exactly: pong-rs-live",
        "Finish this test now.",
    );
    task.max_cycles = 2;
    task.no_tool_policy = NoToolPolicy::WaitUser;

    let result = runtime.run(task).expect("run live agent task");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(result.final_answer.as_deref(), Some("pong-rs-live"));
    assert!(
        result.cycles.iter().any(|cycle| cycle
            .tool_calls
            .iter()
            .any(|call| call.name == "task_finish")),
        "expected the live model to call task_finish"
    );
}

#[test]
#[ignore = "live API call; run with VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_deepseek -- --ignored"]
fn live_deepseek_v4_pro_finishes_sdk_task_without_injected_runtime() {
    if !live_enabled() {
        eprintln!("Live tests are disabled. Set VV_AGENT_RUN_LIVE_TESTS=1 to run.");
        return;
    }

    let settings_path = live_settings_path();
    assert!(
        settings_path.exists(),
        "live settings file is missing: {}",
        settings_path.display()
    );

    let client = AgentSDKClient::new(AgentSDKOptions {
        settings_file: settings_path,
        default_backend: "deepseek".to_string(),
        auto_discover_resources: false,
        ..AgentSDKOptions::default()
    });
    let mut agent = AgentDefinition::default_for_model("deepseek-v4-pro");
    agent.backend = Some("deepseek".to_string());
    agent.max_cycles = 2;
    agent.no_tool_policy = NoToolPolicy::WaitUser;
    agent.system_prompt = Some(
        "You are testing an agent SDK runtime. You must call the task_finish tool. \
         Set the task_finish message to exactly: pong-rs-sdk-live"
            .to_string(),
    );

    let run = client
        .run_with_agent(agent, "Finish this SDK test now.")
        .expect("run live SDK agent task");

    assert_eq!(run.resolved.backend, "deepseek");
    assert_eq!(run.resolved.requested_model, "deepseek-v4-pro");
    assert_eq!(run.result.status, AgentStatus::Completed);
    assert_eq!(run.result.final_answer.as_deref(), Some("pong-rs-sdk-live"));
    assert!(
        run.result.cycles.iter().any(|cycle| cycle
            .tool_calls
            .iter()
            .any(|call| call.name == "task_finish")),
        "expected the live model to call task_finish"
    );
}

#[test]
#[ignore = "live API call; run with VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_deepseek -- --ignored"]
fn live_deepseek_v4_pro_uses_workspace_file_tools() {
    if !live_enabled() {
        eprintln!("Live tests are disabled. Set VV_AGENT_RUN_LIVE_TESTS=1 to run.");
        return;
    }

    let settings_path = live_settings_path();
    assert!(
        settings_path.exists(),
        "live settings file is missing: {}",
        settings_path.display()
    );

    let workspace = tempfile::tempdir().expect("workspace");
    let client = AgentSDKClient::new(AgentSDKOptions {
        settings_file: settings_path,
        default_backend: "deepseek".to_string(),
        workspace: workspace.path().to_path_buf(),
        auto_discover_resources: false,
        ..AgentSDKOptions::default()
    });
    let mut agent = AgentDefinition::default_for_model("deepseek-v4-pro");
    agent.backend = Some("deepseek".to_string());
    agent.language = "en-US".to_string();
    agent.max_cycles = 8;
    agent.no_tool_policy = NoToolPolicy::WaitUser;
    agent.allow_interruption = false;
    agent.enable_todo_management = false;
    agent.use_workspace = true;
    agent.exclude_tools = vec![ASK_USER_TOOL_NAME.to_string()];
    agent.system_prompt = Some(
        "You are running a deterministic integration test for workspace tools.\n\
         Follow this protocol exactly.\n\
         1. First call `write_file` with path `live_workspace_probe.txt` and content \
         exactly `deepseek workspace tool ok`.\n\
         2. After the write succeeds, call `read_file` with path `live_workspace_probe.txt`.\n\
         3. After observing that the read content contains exactly \
         `deepseek workspace tool ok`, call `task_finish` with message exactly \
         `workspace tools observed`.\n\
         Do not answer in plain text before finishing. Do not use bash."
            .to_string(),
    );

    let run = client
        .run_with_agent(agent, "Execute the workspace file tool protocol now.")
        .expect("run live workspace tool test");
    let tool_names = run
        .result
        .cycles
        .iter()
        .flat_map(|cycle| cycle.tool_calls.iter().map(|call| call.name.clone()))
        .collect::<Vec<_>>();

    assert_eq!(run.resolved.backend, "deepseek");
    assert_eq!(run.resolved.requested_model, "deepseek-v4-pro");
    assert_eq!(run.result.status, AgentStatus::Completed, "{tool_names:?}");
    assert_eq!(
        run.result.final_answer.as_deref(),
        Some("workspace tools observed"),
        "{tool_names:?}"
    );
    assert!(
        tool_names.iter().any(|name| name == WRITE_FILE_TOOL_NAME),
        "expected live model to call write_file, got {tool_names:?}"
    );
    assert!(
        tool_names.iter().any(|name| name == READ_FILE_TOOL_NAME),
        "expected live model to call read_file, got {tool_names:?}"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("live_workspace_probe.txt"))
            .expect("workspace probe file"),
        "deepseek workspace tool ok"
    );
}

#[test]
#[ignore = "live API call; run with VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_deepseek -- --ignored"]
fn live_deepseek_v4_pro_observes_background_timeout_handoff() {
    if !live_enabled() {
        eprintln!("Live tests are disabled. Set VV_AGENT_RUN_LIVE_TESTS=1 to run.");
        return;
    }

    let settings_path = live_settings_path();
    assert!(
        settings_path.exists(),
        "live settings file is missing: {}",
        settings_path.display()
    );

    let workspace = tempfile::tempdir().expect("workspace");
    let client = AgentSDKClient::new(AgentSDKOptions {
        settings_file: settings_path,
        default_backend: "deepseek".to_string(),
        workspace: workspace.path().to_path_buf(),
        auto_discover_resources: false,
        ..AgentSDKOptions::default()
    });
    let mut agent = AgentDefinition::default_for_model("deepseek-v4-pro");
    agent.backend = Some("deepseek".to_string());
    agent.language = "en-US".to_string();
    agent.max_cycles = 10;
    agent.no_tool_policy = NoToolPolicy::Continue;
    agent.allow_interruption = true;
    agent.use_workspace = false;
    agent.enable_todo_management = false;
    agent.agent_type = Some("computer".to_string());
    agent.exclude_tools = vec![
        ASK_USER_TOOL_NAME.to_string(),
        CHECK_BACKGROUND_COMMAND_TOOL_NAME.to_string(),
    ];
    agent.system_prompt = Some(
        "You are running a deterministic integration test.\n\
         Follow this protocol exactly.\n\
         1. On your first action, call `bash` exactly once with \
         `command=\"sleep 1.2 && echo BG_DONE\"` and `timeout=1`.\n\
         2. Do not set `run_in_background`.\n\
         3. Never call `check_background_command`.\n\
         4. Do not call `task_finish` until you receive a system notification \
         that the background command completed.\n\
         5. Before that notification arrives, reply with exactly `WAITING` and no tool calls.\n\
         6. After that notification arrives, call `task_finish` with exactly \
         `background observed`.\n\
         Do not deviate from this protocol."
            .to_string(),
    );

    let mut session =
        client.create_session_with_workspace("deepseek-live-bg", agent, workspace.path());
    let events = recorded_events();
    session.subscribe(recording_listener(&events));

    let run = session
        .prompt_with_auto_follow_up(
            "Run the timeout-handoff background notification test.",
            false,
        )
        .expect("run live background handoff test");
    let events = events.lock().expect("events").clone();
    let event_summary = summarize_events(&events);

    assert_eq!(run.resolved.backend, "deepseek", "{event_summary}");
    assert_eq!(
        run.resolved.requested_model, "deepseek-v4-pro",
        "{event_summary}"
    );
    assert_eq!(run.result.status, AgentStatus::Completed, "{event_summary}");
    assert!(
        run.result
            .final_answer
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains("background observed"),
        "{event_summary}"
    );

    assert!(
        events.iter().any(|(event, payload)| {
            event == "tool_result"
                && payload.get("tool_name").and_then(Value::as_str) == Some(BASH_TOOL_NAME)
                && payload
                    .get("metadata")
                    .and_then(Value::as_object)
                    .and_then(|metadata| metadata.get("transitioned_to_background"))
                    .and_then(Value::as_bool)
                    == Some(true)
        }),
        "{event_summary}"
    );
    assert!(
        events.iter().any(|(event, payload)| {
            event == "background_command_completed"
                && payload
                    .get("queued_to_running_session")
                    .and_then(Value::as_bool)
                    == Some(true)
                && payload
                    .get("output")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .contains("BG_DONE")
        }),
        "{event_summary}"
    );
    assert!(
        events
            .iter()
            .any(|(event, _)| event == "session_steer_queued"),
        "{event_summary}"
    );
    assert!(
        events.iter().all(|(event, payload)| {
            event != "tool_result"
                || payload.get("tool_name").and_then(Value::as_str)
                    != Some(CHECK_BACKGROUND_COMMAND_TOOL_NAME)
        }),
        "{event_summary}"
    );
}

type RecordedEvents = Arc<Mutex<Vec<(String, BTreeMap<String, Value>)>>>;
type RecordingListener = Arc<dyn Fn(&str, &BTreeMap<String, Value>) + Send + Sync + 'static>;

fn recorded_events() -> RecordedEvents {
    Arc::new(Mutex::new(Vec::new()))
}

fn recording_listener(events: &RecordedEvents) -> RecordingListener {
    let events = Arc::clone(events);
    Arc::new(move |event, payload| {
        events
            .lock()
            .expect("events lock")
            .push((event.to_string(), payload.clone()));
    })
}

fn summarize_events(events: &[(String, BTreeMap<String, Value>)]) -> String {
    if events.is_empty() {
        return "no session events captured".to_string();
    }

    let mut lines = Vec::new();
    for (event, payload) in events.iter().rev().take(30).rev() {
        let metadata = payload.get("metadata").and_then(Value::as_object);
        let mut summary = BTreeMap::new();
        summary.insert("event".to_string(), Value::String(event.clone()));
        for key in [
            "tool_name",
            "status",
            "session_id",
            "queued_to_running_session",
            "final_answer",
            "wait_reason",
            "error",
            "output",
            "content_preview",
        ] {
            if let Some(value) = payload.get(key).cloned() {
                summary.insert(key.to_string(), value);
            }
        }
        if let Some(metadata) = metadata {
            if let Some(value) = metadata.get("transitioned_to_background").cloned() {
                summary.insert("transitioned_to_background".to_string(), value);
            }
            if let Some(value) = metadata.get("session_id").cloned() {
                summary.insert("metadata_session_id".to_string(), value);
            }
        }
        lines.push(Value::Object(summary.into_iter().collect()).to_string());
    }
    lines.join("\n")
}

fn live_enabled() -> bool {
    env::var("VV_AGENT_RUN_LIVE_TESTS")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn live_settings_path() -> PathBuf {
    env::var("VV_AGENT_LIVE_SETTINGS_JSON")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../third_party_service/vv-llm-rs/crates/vv-llm/tests/fixtures/dev_settings.json")
        })
}
