#![allow(deprecated)]

use std::collections::BTreeMap;

mod common;

use common::{env_string, print_run, runtime_log_handler, ExampleConfig};
use vv_agent::{AgentDefinition, AgentSDKClient, AgentSDKOptions, SubAgentConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let agent_name = env_string("V_AGENT_EXAMPLE_AGENT", "default");
    let mode = env_string("V_AGENT_EXAMPLE_MODE", "run");
    let prompt = config
        .prompt
        .clone()
        .unwrap_or_else(|| "先拆分任务, 再逐步完成并汇报".to_string());

    let client = AgentSDKClient::new_with_agent(
        AgentSDKOptions {
            settings_file: config.settings_file,
            default_backend: config.backend,
            workspace: config.workspace,
            log_handler: runtime_log_handler(config.verbose),
            ..AgentSDKOptions::default()
        },
        default_agent(&config.model),
    );
    let mut client = client;
    client.register_agents(named_agents(&config.model))?;

    let selected = if agent_name == "default" || !client.list_agents().contains(&agent_name) {
        "default".to_string()
    } else {
        agent_name
    };

    if mode == "query" {
        let answer = if selected == "default" {
            client.query_with_require_completed(prompt, false)?
        } else {
            client.query_agent_with_require_completed(selected, prompt, false)?
        };
        println!("{answer}");
    } else {
        let run = if selected == "default" {
            client.run(prompt)?
        } else {
            client.run_agent(selected, prompt)?
        };
        print_run(&run)?;
    }
    Ok(())
}

fn default_agent(model: &str) -> AgentDefinition {
    let mut agent = AgentDefinition::default_for_model(model);
    agent.description = "你是任务规划 Agent, 先拆任务, 再逐步执行并维护 todo.".to_string();
    agent.max_cycles = 10;
    agent.enable_todo_management = true;
    agent
}

fn named_agents(model: &str) -> BTreeMap<String, AgentDefinition> {
    let mut translator = AgentDefinition::default_for_model("MiniMax-M2.5");
    translator.description = "你是专业翻译 Agent, 按段翻译并持续写入目标文件.".to_string();
    translator.backend = Some("minimax".to_string());
    translator.max_cycles = 20;
    translator.enable_todo_management = true;

    let mut orchestrator = AgentDefinition::default_for_model(model);
    orchestrator.description = "你是主控 Agent, 负责把任务分派给已定义的子 Agent.".to_string();
    orchestrator.enable_sub_agents = true;
    orchestrator.sub_agents = BTreeMap::from([
        (
            "research-sub".to_string(),
            SubAgentConfig {
                max_cycles: 8,
                ..SubAgentConfig::new(model, "负责背景检索和资料整理.")
            },
        ),
        (
            "translate-sub".to_string(),
            SubAgentConfig {
                backend: Some("minimax".to_string()),
                max_cycles: 12,
                ..SubAgentConfig::new("MiniMax-M2.5", "负责分段翻译和术语一致性.")
            },
        ),
    ]);

    BTreeMap::from([
        ("translator".to_string(), translator),
        ("orchestrator".to_string(), orchestrator),
    ])
}
