#![allow(deprecated)]

mod common;

use common::{env_string, print_run, session_log_handler, ExampleConfig};
use vv_agent::{AgentDefinition, AgentSDKClient, AgentSDKOptions, AgentStatus};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let user_reply = env_string(
        "V_AGENT_EXAMPLE_USER_REPLY",
        "请使用正式风格, 输出到 artifacts/session_result.md, 长度控制在 5 条以内。",
    );

    let mut agent = AgentDefinition::default_for_model(config.model.clone());
    agent.description = concat!(
        "你是交互式写作 Agent. 在开始执行前, 必须先调用 `ask_user` 收集关键偏好;",
        "拿到用户回复后再继续执行。"
    )
    .to_string();
    agent.backend = Some(config.backend.clone());
    agent.max_cycles = 12;
    agent.enable_todo_management = true;

    let client = AgentSDKClient::new_with_agent(
        AgentSDKOptions {
            settings_file: config.settings_file,
            default_backend: config.backend,
            workspace: config.workspace,
            ..AgentSDKOptions::default()
        },
        agent,
    );
    let mut session = client.create_default_session()?;
    session.subscribe(session_log_handler(config.verbose));

    let first_run = session
        .prompt_with_auto_follow_up("请先询问我输出风格和目标文件, 然后再开始写作计划。", false)?;
    eprintln!("[first_run]");
    print_run(&first_run)?;

    if first_run.result.status == AgentStatus::WaitUser {
        let second_run = session.continue_run(Some(user_reply))?;
        eprintln!("[second_run]");
        print_run(&second_run)?;
    }
    Ok(())
}
