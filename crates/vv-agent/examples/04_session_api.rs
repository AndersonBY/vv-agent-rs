use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;
use vv_agent::{AgentDefinition, AgentSDKClient, AgentSDKOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let settings_file = PathBuf::from(
        env::var("VV_AGENT_LOCAL_SETTINGS").unwrap_or_else(|_| "local_settings.json".to_string()),
    );
    let workspace = PathBuf::from(
        env::var("V_AGENT_EXAMPLE_WORKSPACE").unwrap_or_else(|_| "./workspace".to_string()),
    );
    let backend = env::var("V_AGENT_EXAMPLE_BACKEND").unwrap_or_else(|_| "moonshot".to_string());
    let model = env::var("V_AGENT_EXAMPLE_MODEL").unwrap_or_else(|_| "kimi-k2.5".to_string());
    let verbose = env::var("V_AGENT_EXAMPLE_VERBOSE")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(true);

    std::fs::create_dir_all(&workspace)?;

    let mut agent = AgentDefinition::default_for_model(model);
    agent.description =
        "你是会话型任务助理, 能够持续维护上下文并根据插队消息调整执行策略.".to_string();
    agent.backend = Some(backend.clone());
    agent.max_cycles = 24;
    agent.enable_todo_management = true;
    agent.use_workspace = true;

    let client = AgentSDKClient::new_with_agent(
        AgentSDKOptions {
            settings_file,
            default_backend: backend,
            workspace,
            ..AgentSDKOptions::default()
        },
        agent,
    );
    let mut session = client.create_default_session()?;
    if verbose {
        session.subscribe(Arc::new(on_session_event));
    }

    session.steer("如果读取到 README, 请优先总结 README.")?;
    session.follow_up("上一轮完成后, 再给一个 3 条 bullet 的后续建议.")?;

    let run = session.prompt("请先快速分析 workspace 当前目录结构, 并给出执行建议.")?;
    println!("{}", serde_json::to_string_pretty(&run.to_dict())?);
    Ok(())
}

fn on_session_event(event: &str, payload: &BTreeMap<String, Value>) {
    if matches!(
        event,
        "session_run_start"
            | "cycle_started"
            | "cycle_llm_response"
            | "tool_result"
            | "run_completed"
            | "session_run_end"
            | "session_steer_queued"
            | "session_follow_up_queued"
    ) {
        eprintln!(
            "[{event}] {}",
            Value::Object(payload.clone().into_iter().collect())
        );
    }
}
