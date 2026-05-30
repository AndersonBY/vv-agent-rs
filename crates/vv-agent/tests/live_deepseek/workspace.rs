use vv_agent::constants::{
    ASK_USER_TOOL_NAME, FILE_INFO_TOOL_NAME, FILE_STR_REPLACE_TOOL_NAME, LIST_FILES_TOOL_NAME,
    READ_FILE_TOOL_NAME, WORKSPACE_GREP_TOOL_NAME, WRITE_FILE_TOOL_NAME,
};
use vv_agent::{AgentDefinition, AgentSDKClient, AgentSDKOptions, AgentStatus, NoToolPolicy};

use super::common::{live_enabled, live_settings_path};

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
