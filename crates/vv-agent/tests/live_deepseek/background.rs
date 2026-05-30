use serde_json::Value;
use vv_agent::constants::{ASK_USER_TOOL_NAME, BASH_TOOL_NAME, CHECK_BACKGROUND_COMMAND_TOOL_NAME};
use vv_agent::{AgentDefinition, AgentSDKClient, AgentSDKOptions, AgentStatus, NoToolPolicy};

use super::common::{
    live_enabled, live_settings_path, recorded_events, recording_listener, summarize_events,
};

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
