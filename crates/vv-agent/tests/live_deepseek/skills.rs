use serde_json::Value;
use vv_agent::constants::{ACTIVATE_SKILL_TOOL_NAME, ASK_USER_TOOL_NAME};
use vv_agent::{AgentDefinition, AgentSDKClient, AgentSDKOptions, AgentStatus, NoToolPolicy};

use super::common::{live_enabled, live_settings_path};

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
