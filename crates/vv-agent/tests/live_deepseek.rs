use std::env;
use std::path::PathBuf;

use vv_agent::{handoff, Agent, AgentStatus, ModelRef, RunConfig, Runner, VvLlmModelProvider};

#[path = "live_support/deepseek_accounting.rs"]
mod deepseek_accounting;

#[tokio::test]
#[ignore = "live API call; run with VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_deepseek -- --ignored"]
async fn live_deepseek_v4_pro_finishes_runner_task() {
    if !live_enabled() {
        eprintln!("set VV_AGENT_RUN_LIVE_TESTS=1 to run live DeepSeek Agent/Runner tests");
        return;
    }
    let (runner, model) = live_runner().expect("runner");
    let agent = live_agent("deepseek-runner", "你是执行 Agent。直接完成任务。", &model);

    let result = runner
        .run_with_config(
            &agent,
            "用一句话回答：vv-agent-rs 的 SDK 入口是什么？",
            RunConfig::builder().max_cycles(6).build(),
        )
        .await
        .expect("run live task");

    assert_eq!(result.status(), AgentStatus::Completed);
    assert!(result.final_output().unwrap_or_default().contains("Agent"));
}

#[tokio::test]
#[ignore = "live API call; run with VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_deepseek -- --ignored"]
async fn live_deepseek_v4_pro_runs_handoff_facade() {
    if !live_enabled() {
        eprintln!("set VV_AGENT_RUN_LIVE_TESTS=1 to run live DeepSeek Agent/Runner tests");
        return;
    }
    let (runner, model) = live_runner().expect("runner");
    let researcher = live_agent("researcher", "你收集事实并简短回答。", &model);
    let triage = Agent::builder("triage")
        .instructions("需要事实回答时转交 researcher。")
        .model(model)
        .handoff(handoff(&researcher).description("事实收集任务"))
        .build()
        .expect("triage agent");

    let result = runner
        .run_with_config(
            &triage,
            "研究并回答：Runner 负责什么？",
            RunConfig::builder().max_cycles(8).build(),
        )
        .await
        .expect("run handoff");

    assert_eq!(result.status(), AgentStatus::Completed);
}

#[tokio::test]
#[ignore = "live API call; run with VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_deepseek -- --ignored"]
async fn live_deepseek_v4_pro_starts_background_agent_task() {
    if !live_enabled() {
        eprintln!("set VV_AGENT_RUN_LIVE_TESTS=1 to run live DeepSeek Agent/Runner tests");
        return;
    }
    let (runner, model) = live_runner().expect("runner");
    let agent = live_agent("background-worker", "你在后台执行简短任务。", &model);
    let task = agent
        .as_background_task()
        .name("live_background_worker")
        .build()
        .expect("background task");
    let workspace = std::env::temp_dir().join("vv-agent-live-background");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let context = vv_agent::ToolContext::new(workspace);

    let handle = task
        .start(
            &runner,
            &context,
            serde_json::json!({"task_description": "用一句话说明后台任务已经启动"}),
            None,
        )
        .expect("start background task");

    assert!(!handle.task_id().is_empty());
}

fn live_runner() -> Result<(Runner, ModelRef), String> {
    let model_name = env::var("VV_AGENT_LIVE_MODEL").unwrap_or_else(|_| "deepseek-v4-pro".into());
    let model = ModelRef::backend("deepseek", model_name);
    let provider = VvLlmModelProvider::from_settings_file(live_settings_path())
        .with_default_backend("deepseek");
    let workspace = std::env::temp_dir().join("vv-agent-live-deepseek");
    std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(workspace)
        .build()?;
    Ok((runner, model))
}

fn live_agent(name: &str, instructions: &str, model: &ModelRef) -> Agent {
    Agent::builder(name)
        .instructions(instructions)
        .model(model.clone())
        .build()
        .expect("agent")
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
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/dev_settings.json")
        })
}
