use std::env;
use std::path::PathBuf;

use vv_agent::{AgentDefinition, AgentResourceLoader, AgentSDKClient, AgentSDKOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let settings_file = PathBuf::from(
        env::var("V_AGENT_LOCAL_SETTINGS").unwrap_or_else(|_| "local_settings.py".to_string()),
    );
    let workspace = PathBuf::from(
        env::var("V_AGENT_EXAMPLE_WORKSPACE").unwrap_or_else(|_| "./workspace".to_string()),
    );
    let backend = env::var("V_AGENT_EXAMPLE_BACKEND").unwrap_or_else(|_| "moonshot".to_string());
    let model = env::var("V_AGENT_EXAMPLE_MODEL").unwrap_or_else(|_| "kimi-k2.5".to_string());
    let prompt = env::var("V_AGENT_EXAMPLE_PROMPT")
        .unwrap_or_else(|_| "请概述这个项目的关键能力。".to_string());

    std::fs::create_dir_all(&workspace)?;

    let loader = AgentResourceLoader::new(&workspace);
    let mut default_agent = AgentDefinition::default_for_model(model);
    default_agent.description = "你是默认 Agent, 当未提供 profile 时用于兜底.".to_string();
    default_agent.backend = Some(backend.clone());
    default_agent.max_cycles = 16;
    default_agent.enable_todo_management = true;

    let client = AgentSDKClient::new_with_agent(
        AgentSDKOptions {
            settings_file,
            default_backend: backend,
            workspace,
            resource_loader: Some(loader),
            ..AgentSDKOptions::default()
        },
        default_agent,
    );

    eprintln!("[discovered agents] {:?}", client.list_agents());
    for diagnostic in client.resource_diagnostics() {
        eprintln!("- {diagnostic}");
    }

    let selected_agent = env::var("V_AGENT_EXAMPLE_AGENT").unwrap_or_default();
    let run = if !selected_agent.trim().is_empty()
        && client
            .list_agents()
            .iter()
            .any(|name| name == &selected_agent)
    {
        client.run_agent(selected_agent, prompt)?
    } else {
        client.run(prompt)?
    };
    println!("{}", serde_json::to_string_pretty(&run.to_dict())?);
    Ok(())
}
