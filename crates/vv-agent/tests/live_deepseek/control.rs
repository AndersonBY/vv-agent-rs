use vv_agent::constants::ASK_USER_TOOL_NAME;
use vv_agent::{
    build_vv_llm_from_local_settings, AgentDefinition, AgentRuntime, AgentSDKClient,
    AgentSDKOptions, AgentStatus, AgentTask, NoToolPolicy,
};

use super::common::{live_enabled, live_settings_path};

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
