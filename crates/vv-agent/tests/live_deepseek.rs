use std::env;
use std::path::PathBuf;

use vv_agent::{
    build_openai_llm_from_local_settings, AgentRuntime, AgentStatus, AgentTask, NoToolPolicy,
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
        build_openai_llm_from_local_settings(&settings_path, "deepseek", "deepseek-v4-pro", 90.0)
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
