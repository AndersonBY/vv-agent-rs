use serde_json::Value;
use vv_agent::constants::{
    ASK_USER_TOOL_NAME, COMPRESS_MEMORY_TOOL_NAME, FILE_INFO_TOOL_NAME, FILE_STR_REPLACE_TOOL_NAME,
    LIST_FILES_TOOL_NAME, READ_FILE_TOOL_NAME, TODO_WRITE_TOOL_NAME, WORKSPACE_GREP_TOOL_NAME,
    WRITE_FILE_TOOL_NAME,
};
use vv_agent::{AgentDefinition, AgentSDKClient, AgentSDKOptions, AgentStatus, NoToolPolicy};

use super::common::{live_enabled, live_settings_path};

#[test]
#[ignore = "live API call; run with VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_deepseek -- --ignored"]
fn live_deepseek_v4_pro_uses_todo_write_protocol() {
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
    agent.enable_todo_management = true;
    agent.use_workspace = true;
    agent.exclude_tools = vec![
        ASK_USER_TOOL_NAME.to_string(),
        LIST_FILES_TOOL_NAME.to_string(),
        FILE_INFO_TOOL_NAME.to_string(),
        READ_FILE_TOOL_NAME.to_string(),
        WRITE_FILE_TOOL_NAME.to_string(),
        FILE_STR_REPLACE_TOOL_NAME.to_string(),
        WORKSPACE_GREP_TOOL_NAME.to_string(),
    ];
    agent.system_prompt = Some(
        "You are running a deterministic integration test for TODO tools.\n\
         Follow this protocol exactly.\n\
         1. First call `todo_write` with exactly one todo: title `live todo protocol`, \
         status `in_progress`, priority `high`.\n\
         2. After that succeeds, call `todo_write` again with exactly one todo: \
         title `live todo protocol`, status `completed`, priority `high`.\n\
         3. Only after observing the completed TODO list, call `task_finish` with \
         message exactly `todo tools observed`.\n\
         Do not answer in plain text before finishing. Do not use file tools."
            .to_string(),
    );

    let run = client
        .run_with_agent(agent, "Execute the TODO tool protocol now.")
        .expect("run live TODO tool test");
    let tool_names = run
        .result
        .cycles
        .iter()
        .flat_map(|cycle| cycle.tool_calls.iter().map(|call| call.name.clone()))
        .collect::<Vec<_>>();
    let todo_result_payloads = run
        .result
        .cycles
        .iter()
        .flat_map(|cycle| cycle.tool_results.iter())
        .filter(|result| result.content.contains("live todo protocol"))
        .map(|result| result.content.clone())
        .collect::<Vec<_>>();

    assert_eq!(run.resolved.backend, "deepseek");
    assert_eq!(run.resolved.requested_model, "deepseek-v4-pro");
    assert_eq!(run.result.status, AgentStatus::Completed, "{tool_names:?}");
    assert_eq!(
        run.result.final_answer.as_deref(),
        Some("todo tools observed"),
        "{tool_names:?}"
    );
    assert!(
        tool_names.iter().any(|name| name == TODO_WRITE_TOOL_NAME),
        "expected live model to call todo_write, got {tool_names:?}"
    );
    assert!(
        todo_result_payloads
            .iter()
            .any(|payload| payload.contains("\"status\":\"in_progress\"")),
        "expected in_progress todo payload, got {todo_result_payloads:?}"
    );
    assert!(
        todo_result_payloads
            .iter()
            .any(|payload| payload.contains("\"status\":\"completed\"")),
        "expected completed todo payload, got {todo_result_payloads:?}"
    );
}

#[test]
#[ignore = "live API call; run with VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_deepseek -- --ignored"]
fn live_deepseek_v4_pro_uses_compress_memory_tool() {
    if !live_enabled() {
        eprintln!("Live tests are disabled. Set VV_AGENT_RUN_LIVE_TESTS=1 to run.");
        return;
    }

    let workspace = tempfile::tempdir().expect("workspace");
    let client = AgentSDKClient::new(AgentSDKOptions {
        settings_file: live_settings_path(),
        default_backend: "deepseek".to_string(),
        workspace: workspace.path().to_path_buf(),
        auto_discover_resources: false,
        ..AgentSDKOptions::default()
    });
    let mut agent = AgentDefinition::default_for_model("deepseek-v4-pro");
    agent.backend = Some("deepseek".to_string());
    agent.language = "en-US".to_string();
    agent.max_cycles = 5;
    agent.no_tool_policy = NoToolPolicy::WaitUser;
    agent.allow_interruption = false;
    agent.enable_todo_management = false;
    agent.use_workspace = true;
    agent.exclude_tools = vec![
        ASK_USER_TOOL_NAME.to_string(),
        TODO_WRITE_TOOL_NAME.to_string(),
        LIST_FILES_TOOL_NAME.to_string(),
        FILE_INFO_TOOL_NAME.to_string(),
        READ_FILE_TOOL_NAME.to_string(),
        WRITE_FILE_TOOL_NAME.to_string(),
        FILE_STR_REPLACE_TOOL_NAME.to_string(),
        WORKSPACE_GREP_TOOL_NAME.to_string(),
    ];
    agent.system_prompt = Some(
        "You are running a deterministic integration test for memory tools.\n\
         First call `compress_memory` with core_information exactly `live memory note preserved`.\n\
         After it succeeds, call `task_finish` with message exactly `memory tools observed`.\n\
         Do not answer in plain text before finishing."
            .to_string(),
    );

    let run = client
        .run_with_agent(agent, "Execute the memory note protocol now.")
        .expect("run live memory tool test");
    let tool_names = run
        .result
        .cycles
        .iter()
        .flat_map(|cycle| cycle.tool_calls.iter().map(|call| call.name.clone()))
        .collect::<Vec<_>>();
    let memory_notes = run
        .result
        .shared_state
        .get("memory_notes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    assert_eq!(run.result.status, AgentStatus::Completed, "{tool_names:?}");
    assert_eq!(
        run.result.final_answer.as_deref(),
        Some("memory tools observed")
    );
    assert!(tool_names
        .iter()
        .any(|name| name == COMPRESS_MEMORY_TOOL_NAME));
    assert!(
        memory_notes
            .iter()
            .any(|note| note.get("core_information").and_then(Value::as_str)
                == Some("live memory note preserved")),
        "expected memory note in shared_state: {memory_notes:?}"
    );
}
