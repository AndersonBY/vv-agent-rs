use std::collections::BTreeMap;

use vv_agent::constants::{
    ASK_USER_TOOL_NAME, CREATE_SUB_TASK_TOOL_NAME, SUB_TASK_STATUS_TOOL_NAME,
};
use vv_agent::{
    AgentDefinition, AgentSDKClient, AgentSDKOptions, AgentStatus, NoToolPolicy, SubAgentConfig,
};

use super::common::{live_enabled, live_settings_path};

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
fn live_deepseek_v4_pro_polls_async_sub_agent_status() {
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
        "A deterministic async sub-agent used only for live status verification.",
    );
    sub_agent.backend = Some("deepseek".to_string());
    sub_agent.max_cycles = 3;
    sub_agent.system_prompt = Some(
        "You are the async delegated sub-agent in a deterministic integration test. \
         You must call `task_finish` with message exactly: async sub-agent live result"
            .to_string(),
    );

    let mut agent = AgentDefinition::default_for_model("deepseek-v4-pro");
    agent.backend = Some("deepseek".to_string());
    agent.language = "en-US".to_string();
    agent.max_cycles = 12;
    agent.no_tool_policy = NoToolPolicy::WaitUser;
    agent.allow_interruption = false;
    agent.enable_todo_management = false;
    agent.use_workspace = false;
    agent.enable_sub_agents = true;
    agent.sub_agents = BTreeMap::from([("async-sub".to_string(), sub_agent)]);
    agent.exclude_tools = vec![ASK_USER_TOOL_NAME.to_string()];
    agent.system_prompt = Some(
        "You are running a deterministic integration test for async sub-agent status polling.\n\
         Follow this protocol exactly.\n\
         1. First call `create_sub_task` with `agent_id` exactly `async-sub`, \
         `task_description` exactly `Return the async live delegation token now.`, \
         `output_requirements` exactly `The sub-agent final answer must be async sub-agent live result`, \
         and `wait_for_completion=false`.\n\
         2. After `create_sub_task` returns a `task_id`, call `sub_task_status` \
         with `task_ids` containing exactly that task_id and `detail_level` exactly `snapshot`.\n\
         3. If the status is not completed yet, call `sub_task_status` again with the same task_id.\n\
         4. After `sub_task_status` returns a completed task whose `final_answer` is \
         `async sub-agent live result`, call `task_finish` with message exactly \
         `async sub-agent status observed`.\n\
         Do not answer in plain text before finishing."
            .to_string(),
    );

    let run = client
        .run_with_agent(agent, "Execute the async sub-agent status protocol now.")
        .expect("run live async sub-agent status test");
    let tool_names = run
        .result
        .cycles
        .iter()
        .flat_map(|cycle| cycle.tool_calls.iter().map(|call| call.name.clone()))
        .collect::<Vec<_>>();
    let status_payload = run
        .result
        .cycles
        .iter()
        .flat_map(|cycle| cycle.tool_results.iter())
        .find(|result| {
            result.content.contains("async sub-agent live result")
                && result.content.contains("\"detail_level\":\"snapshot\"")
        })
        .map(|result| result.content.clone());

    assert_eq!(run.resolved.backend, "deepseek");
    assert_eq!(run.resolved.requested_model, "deepseek-v4-pro");
    assert_eq!(run.result.status, AgentStatus::Completed, "{tool_names:?}");
    assert_eq!(
        run.result.final_answer.as_deref(),
        Some("async sub-agent status observed"),
        "{tool_names:?}"
    );
    assert!(
        tool_names
            .iter()
            .any(|name| name == CREATE_SUB_TASK_TOOL_NAME),
        "expected live model to call create_sub_task, got {tool_names:?}"
    );
    assert!(
        tool_names
            .iter()
            .any(|name| name == SUB_TASK_STATUS_TOOL_NAME),
        "expected live model to call sub_task_status, got {tool_names:?}"
    );
    let status_payload =
        status_payload.expect("sub_task_status result should include async final answer");
    assert!(
        status_payload.contains("\"status\":\"completed\"")
            || status_payload.contains("\"status\":\"Completed\""),
        "unexpected async status payload: {status_payload}"
    );
}
