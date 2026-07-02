use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;

use serde_json::{json, Value};
use vv_agent::prompt::{build_system_prompt_with_options, BuildSystemPromptOptions};
use vv_agent::{
    build_default_registry, build_vv_llm_from_local_settings, AgentRuntime, AgentStatus, AgentTask,
    SubAgentConfig,
};

#[test]
#[ignore = "live API call; run with VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_sub_task_wait -- --ignored"]
fn live_agent_waits_for_background_sub_task_completion() {
    if !live_enabled() {
        eprintln!("Live tests are disabled. Set VV_AGENT_RUN_LIVE_TESTS=1 to run.");
        return;
    }

    let settings_file = live_settings_path();
    assert!(
        settings_file.exists(),
        "live settings file is missing: {}",
        settings_file.display()
    );
    let backend = env::var("VV_AGENT_LIVE_BACKEND").unwrap_or_else(|_| "moonshot".to_string());
    let model = env::var("VV_AGENT_LIVE_MODEL").unwrap_or_else(|_| "kimi-k2.6".to_string());
    let workspace = env::temp_dir().join("vv-agent-rs-live-sub-task-wait");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let (llm, resolved) = build_vv_llm_from_local_settings(&settings_file, &backend, &model, 90.0)
        .expect("build live vv-llm client");
    let mut runtime = AgentRuntime::new(llm)
        .with_tool_registry(build_default_registry())
        .with_settings_file(settings_file)
        .with_default_backend(backend.clone())
        .with_sub_agent_timeout_seconds(90.0);
    runtime.default_workspace = Some(workspace.clone());

    let mut available_sub_agents = BTreeMap::new();
    available_sub_agents.insert(
        "slow-researcher".to_string(),
        "Sleeps briefly, then returns the requested token.".to_string(),
    );
    let parent_prompt = build_system_prompt_with_options(
        "You are testing sub-agent orchestration. Follow these exact steps and do not answer directly. \
Step 1: call create_sub_task with agent_id='slow-researcher', task_description asking the child \
to sleep briefly then return the token WAIT-SUBTASK-OK, and wait_for_completion=false. \
Step 2: after you receive the task_id, call sub_task_status with that task_id, wait_for_completion=true, \
check_interval_seconds=300, max_wait_seconds=120, and detail_level='snapshot'. \
Step 3: after the status result is completed, call task_finish with a message containing WAIT-SUBTASK-OK.",
        BuildSystemPromptOptions {
            language: "en-US".to_string(),
            available_sub_agents,
            workspace: Some(workspace.clone()),
            ..BuildSystemPromptOptions::default()
        },
    );
    let child_prompt = "You are the slow-researcher sub-agent. First call bash with command 'sleep 2' and timeout 10. \
After bash succeeds, call task_finish with exactly: WAIT-SUBTASK-OK.";

    let mut sub_agent =
        SubAgentConfig::new(model.clone(), "Sleeps briefly and returns WAIT-SUBTASK-OK.");
    sub_agent.backend = Some(backend);
    sub_agent.system_prompt = Some(child_prompt.to_string());
    sub_agent.max_cycles = 4;

    let mut task = AgentTask::new(
        "live_sub_task_wait",
        resolved.model_id,
        parent_prompt,
        "Run the exact live sub-task wait verification now.",
    );
    task.max_cycles = 8;
    task.sub_agents
        .insert("slow-researcher".to_string(), sub_agent);
    task.metadata
        .insert("language".to_string(), Value::String("en-US".to_string()));

    let result = runtime.run(task).expect("run live task");

    assert_eq!(result.status, AgentStatus::Completed, "{:?}", result.error);
    let tool_calls = result
        .cycles
        .iter()
        .flat_map(|cycle| cycle.tool_calls.iter())
        .collect::<Vec<_>>();
    let create_calls = tool_calls
        .iter()
        .filter(|call| call.name == "create_sub_task")
        .collect::<Vec<_>>();
    let status_calls = tool_calls
        .iter()
        .filter(|call| call.name == "sub_task_status")
        .collect::<Vec<_>>();
    assert!(
        !create_calls.is_empty(),
        "live parent did not call create_sub_task"
    );
    assert!(
        !status_calls.is_empty(),
        "live parent did not call sub_task_status"
    );
    assert_eq!(
        create_calls[0].arguments.get("wait_for_completion"),
        Some(&json!(false))
    );
    assert!(status_calls
        .iter()
        .any(|call| call.arguments.get("wait_for_completion") == Some(&json!(true))));

    let status_call_ids = status_calls
        .iter()
        .map(|call| call.id.as_str())
        .collect::<Vec<_>>();
    let status_payloads = result
        .cycles
        .iter()
        .flat_map(|cycle| cycle.tool_results.iter())
        .filter(|result| status_call_ids.contains(&result.tool_call_id.as_str()))
        .map(|result| serde_json::from_str::<Value>(&result.content).expect("status payload"))
        .collect::<Vec<_>>();
    assert!(
        !status_payloads.is_empty(),
        "live run did not return sub_task_status payload"
    );
    let completed_status = status_payloads.iter().find(|payload| {
        payload["wait_for_completion"] == json!(true)
            && payload["wait_exceeded"] == json!(false)
            && payload["tasks"][0]["status"] == json!("completed")
    });
    assert!(
        completed_status.is_some(),
        "status payloads did not include completed wait result: {status_payloads:?}"
    );
    let completed_status = completed_status.expect("completed status payload");
    assert_eq!(completed_status["running_task_ids"], json!([]));
    assert_eq!(
        completed_status["suggested_next_check_after_seconds"],
        json!(300)
    );
    assert!(serde_json::to_string(completed_status)
        .expect("status json")
        .contains("WAIT-SUBTASK-OK"));
    assert!(result
        .final_answer
        .as_deref()
        .unwrap_or_default()
        .contains("WAIT-SUBTASK-OK"));
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
