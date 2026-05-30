use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use vv_agent::constants::{
    ACTIVATE_SKILL_TOOL_NAME, ASK_USER_TOOL_NAME, BASH_TOOL_NAME,
    CHECK_BACKGROUND_COMMAND_TOOL_NAME, COMPRESS_MEMORY_TOOL_NAME, CREATE_SUB_TASK_TOOL_NAME,
    FILE_INFO_TOOL_NAME, FILE_STR_REPLACE_TOOL_NAME, LIST_FILES_TOOL_NAME, READ_FILE_TOOL_NAME,
    TODO_WRITE_TOOL_NAME, WORKSPACE_GREP_TOOL_NAME, WRITE_FILE_TOOL_NAME,
};
use vv_agent::{
    build_vv_llm_from_local_settings, AgentDefinition, AgentRuntime, AgentSDKClient,
    AgentSDKOptions, AgentStatus, AgentTask, NoToolPolicy, SubAgentConfig,
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
fn live_deepseek_v4_pro_requests_user_input_with_ask_user() {
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
    agent.max_cycles = 3;
    agent.no_tool_policy = NoToolPolicy::WaitUser;
    agent.allow_interruption = true;
    agent.enable_todo_management = false;
    agent.use_workspace = false;
    agent.system_prompt = Some(
        "You are running a deterministic integration test for user-input pauses.\n\
         Follow this protocol exactly.\n\
         1. Call `ask_user` with question exactly `Choose live option?`, options \
         exactly [`alpha`, `beta`], selection_type exactly `single`, and \
         allow_custom_options=false.\n\
         2. Do not call `task_finish`.\n\
         3. Do not answer in plain text before calling the tool."
            .to_string(),
    );

    let run = client
        .run_with_agent(agent, "Request the user decision now.")
        .expect("run live ask_user test");
    let tool_names = run
        .result
        .cycles
        .iter()
        .flat_map(|cycle| cycle.tool_calls.iter().map(|call| call.name.clone()))
        .collect::<Vec<_>>();

    assert_eq!(run.resolved.backend, "deepseek");
    assert_eq!(run.resolved.requested_model, "deepseek-v4-pro");
    assert_eq!(run.result.status, AgentStatus::WaitUser, "{tool_names:?}");
    assert!(
        run.result
            .wait_reason
            .as_deref()
            .unwrap_or_default()
            .contains("Choose live option?"),
        "wait_reason was {:?}",
        run.result.wait_reason
    );
    assert!(
        tool_names.iter().any(|name| name == ASK_USER_TOOL_NAME),
        "expected live model to call ask_user, got {tool_names:?}"
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

#[test]
#[ignore = "live API call; run with VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_deepseek -- --ignored"]
fn live_deepseek_v4_pro_activates_available_skill() {
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
    let skill_dir = workspace.path().join("skills/live-skill");
    std::fs::create_dir_all(&skill_dir).expect("skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: live-skill\ndescription: Deterministic live skill\n---\n\
         When this skill is active, finish with exactly: skill tools observed\n",
    )
    .expect("skill file");

    let client = AgentSDKClient::new(AgentSDKOptions {
        settings_file: settings_path,
        default_backend: "deepseek".to_string(),
        workspace: workspace.path().to_path_buf(),
        auto_discover_resources: false,
        ..AgentSDKOptions::default()
    });
    let mut agent = AgentDefinition::default_for_model("deepseek-v4-pro");
    agent.backend = Some("deepseek".to_string());
    agent.description = "Deterministic skill activation live test. First call `activate_skill` \
        with skill_name `live-skill` and reason `live verification`. After reading the returned \
        instructions, call `task_finish` with message exactly `skill tools observed`."
        .to_string();
    agent.language = "en-US".to_string();
    agent.max_cycles = 5;
    agent.no_tool_policy = NoToolPolicy::WaitUser;
    agent.allow_interruption = false;
    agent.enable_todo_management = false;
    agent.use_workspace = false;
    agent.skill_directories = vec!["skills".to_string()];
    agent.exclude_tools = vec![ASK_USER_TOOL_NAME.to_string()];

    let run = client
        .run_with_agent(agent, "Execute the skill activation protocol now.")
        .expect("run live skill activation test");
    let tool_names = run
        .result
        .cycles
        .iter()
        .flat_map(|cycle| cycle.tool_calls.iter().map(|call| call.name.clone()))
        .collect::<Vec<_>>();
    let activated_skills = run
        .result
        .shared_state
        .get("active_skills")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    assert_eq!(run.resolved.backend, "deepseek");
    assert_eq!(run.resolved.requested_model, "deepseek-v4-pro");
    assert_eq!(run.result.status, AgentStatus::Completed, "{tool_names:?}");
    assert_eq!(
        run.result.final_answer.as_deref(),
        Some("skill tools observed"),
        "{tool_names:?}"
    );
    assert!(
        tool_names
            .iter()
            .any(|name| name == ACTIVATE_SKILL_TOOL_NAME),
        "expected live model to call activate_skill, got {tool_names:?}"
    );
    assert!(
        activated_skills
            .iter()
            .any(|skill| skill.as_str() == Some("live-skill")),
        "expected active_skills to include live-skill, got {activated_skills:?}"
    );
}

#[test]
#[ignore = "live API call; run with VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_deepseek -- --ignored"]
fn live_deepseek_v4_pro_uses_surgical_file_edit_tool() {
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
    let target_path = workspace.path().join("live_edit_target.txt");
    std::fs::write(&target_path, "status = draft\nnotes = keep\n").expect("seed edit target");

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
    agent.exclude_tools = vec![
        ASK_USER_TOOL_NAME.to_string(),
        WRITE_FILE_TOOL_NAME.to_string(),
    ];
    agent.system_prompt = Some(
        "You are running a deterministic integration test for surgical file edits.\n\
         Follow this protocol exactly.\n\
         1. First call `read_file` with path `live_edit_target.txt`.\n\
         2. After reading the file, call `file_str_replace` with path \
         `live_edit_target.txt`, old_str exactly `status = draft`, and new_str \
         exactly `status = shipped`.\n\
         3. After the replacement succeeds, call `read_file` with path \
         `live_edit_target.txt`.\n\
         4. After observing that the file contains `status = shipped` and still \
         contains `notes = keep`, call `task_finish` with message exactly \
         `edit tools observed`.\n\
         Do not answer in plain text before finishing. Do not use bash."
            .to_string(),
    );

    let run = client
        .run_with_agent(agent, "Execute the surgical file edit protocol now.")
        .expect("run live file edit tool test");
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
        Some("edit tools observed"),
        "{tool_names:?}"
    );
    assert!(
        tool_names.iter().any(|name| name == READ_FILE_TOOL_NAME),
        "expected live model to call read_file, got {tool_names:?}"
    );
    assert!(
        tool_names
            .iter()
            .any(|name| name == FILE_STR_REPLACE_TOOL_NAME),
        "expected live model to call file_str_replace, got {tool_names:?}"
    );
    assert_eq!(
        std::fs::read_to_string(target_path).expect("edited target file"),
        "status = shipped\nnotes = keep\n"
    );
}

#[test]
#[ignore = "live API call; run with VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_deepseek -- --ignored"]
fn live_deepseek_v4_pro_uses_workspace_discovery_tools() {
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
    std::fs::write(
        workspace.path().join("search_target.txt"),
        "title = live-search\nneedle_live_search_token = present\n",
    )
    .expect("seed search target");
    std::fs::write(workspace.path().join("other.txt"), "title = no-match\n")
        .expect("seed non-match file");

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
    agent.exclude_tools = vec![
        ASK_USER_TOOL_NAME.to_string(),
        READ_FILE_TOOL_NAME.to_string(),
        WRITE_FILE_TOOL_NAME.to_string(),
        FILE_STR_REPLACE_TOOL_NAME.to_string(),
    ];
    agent.system_prompt = Some(
        "You are running a deterministic integration test for workspace discovery tools.\n\
         Follow this protocol exactly.\n\
         1. First call `list_files` with path `.` and glob `**/*.txt`.\n\
         2. After observing `search_target.txt`, call `workspace_grep` with pattern \
         `needle_live_search_token`, path `.`, glob `**/*.txt`, output_mode \
         `content`, and n=true.\n\
         3. After observing the grep match in `search_target.txt`, call `file_info` \
         with path `search_target.txt`.\n\
         4. After observing file metadata for `search_target.txt`, call `task_finish` \
         with message exactly `discovery tools observed`.\n\
         Do not answer in plain text before finishing. Do not use bash."
            .to_string(),
    );

    let run = client
        .run_with_agent(agent, "Execute the workspace discovery protocol now.")
        .expect("run live workspace discovery tool test");
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
        Some("discovery tools observed"),
        "{tool_names:?}"
    );
    assert!(
        tool_names.iter().any(|name| name == LIST_FILES_TOOL_NAME),
        "expected live model to call list_files, got {tool_names:?}"
    );
    assert!(
        tool_names
            .iter()
            .any(|name| name == WORKSPACE_GREP_TOOL_NAME),
        "expected live model to call workspace_grep, got {tool_names:?}"
    );
    assert!(
        tool_names.iter().any(|name| name == FILE_INFO_TOOL_NAME),
        "expected live model to call file_info, got {tool_names:?}"
    );
}

#[test]
#[ignore = "live API call; run with VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_deepseek -- --ignored"]
fn live_deepseek_v4_pro_runs_configured_sub_agent() {
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
    let mut sub_agent = SubAgentConfig::new(
        "deepseek-v4-pro",
        "A deterministic sub-agent used only for live delegation verification.",
    );
    sub_agent.backend = Some("deepseek".to_string());
    sub_agent.max_cycles = 3;
    sub_agent.system_prompt = Some(
        "You are the delegated sub-agent in a deterministic integration test. \
         You must call `task_finish` with message exactly: sub-agent live result"
            .to_string(),
    );

    let mut agent = AgentDefinition::default_for_model("deepseek-v4-pro");
    agent.backend = Some("deepseek".to_string());
    agent.language = "en-US".to_string();
    agent.max_cycles = 8;
    agent.no_tool_policy = NoToolPolicy::WaitUser;
    agent.allow_interruption = false;
    agent.enable_todo_management = false;
    agent.use_workspace = false;
    agent.enable_sub_agents = true;
    agent.sub_agents = BTreeMap::from([("research-sub".to_string(), sub_agent)]);
    agent.exclude_tools = vec![ASK_USER_TOOL_NAME.to_string()];
    agent.system_prompt = Some(
        "You are running a deterministic integration test for sub-agent delegation.\n\
         Follow this protocol exactly.\n\
         1. First call `create_sub_task` with `agent_id` exactly `research-sub`, \
         `task_description` exactly `Return the live delegation token now.`, and \
         `output_requirements` exactly `The sub-agent final answer must be sub-agent live result`.\n\
         2. After `create_sub_task` returns a completed result whose `final_answer` is \
         `sub-agent live result`, call `task_finish` with message exactly `sub-agent observed`.\n\
         Do not answer in plain text before finishing."
            .to_string(),
    );

    let run = client
        .run_with_agent(agent, "Execute the sub-agent delegation protocol now.")
        .expect("run live sub-agent delegation test");
    let tool_names = run
        .result
        .cycles
        .iter()
        .flat_map(|cycle| cycle.tool_calls.iter().map(|call| call.name.clone()))
        .collect::<Vec<_>>();
    let sub_task_result = run
        .result
        .cycles
        .iter()
        .flat_map(|cycle| cycle.tool_results.iter())
        .find(|result| result.content.contains("sub-agent live result"))
        .map(|result| result.content.clone());

    assert_eq!(run.resolved.backend, "deepseek");
    assert_eq!(run.resolved.requested_model, "deepseek-v4-pro");
    assert_eq!(run.result.status, AgentStatus::Completed, "{tool_names:?}");
    assert_eq!(
        run.result.final_answer.as_deref(),
        Some("sub-agent observed"),
        "{tool_names:?}"
    );
    assert!(
        tool_names
            .iter()
            .any(|name| name == CREATE_SUB_TASK_TOOL_NAME),
        "expected live model to call create_sub_task, got {tool_names:?}"
    );
    let sub_task_payload =
        sub_task_result.expect("create_sub_task result should include sub-agent final answer");
    assert!(
        sub_task_payload.contains("\"status\":\"completed\"")
            || sub_task_payload.contains("\"status\":\"Completed\""),
        "unexpected sub-task payload: {sub_task_payload}"
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
