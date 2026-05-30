use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;

use vv_agent::{AgentDefinition, AgentSDKClient, AgentSDKOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let settings_file = PathBuf::from(
        env::var("VV_AGENT_LOCAL_SETTINGS").unwrap_or_else(|_| "local_settings.json".to_string()),
    );
    let workspace = PathBuf::from(
        env::var("V_AGENT_EXAMPLE_WORKSPACE").unwrap_or_else(|_| "./workspace".to_string()),
    );
    let default_backend =
        env::var("V_AGENT_EXAMPLE_BACKEND").unwrap_or_else(|_| "moonshot".to_string());
    let profile_name =
        env::var("V_AGENT_EXAMPLE_PROFILE").unwrap_or_else(|_| "researcher".to_string());
    let prompt = env::var("V_AGENT_EXAMPLE_PROMPT")
        .unwrap_or_else(|_| "分析 workspace 下这个文档的核心结论".to_string());

    std::fs::create_dir_all(&workspace)?;

    let client = AgentSDKClient::new_with_agents(
        AgentSDKOptions {
            settings_file,
            default_backend,
            workspace,
            ..AgentSDKOptions::default()
        },
        profiles(),
    )?;

    let agent_name = if client
        .list_agents()
        .iter()
        .any(|name| name == &profile_name)
    {
        profile_name
    } else {
        "researcher".to_string()
    };
    let run = client.run_agent(&agent_name, prompt)?;
    println!("{}", serde_json::to_string_pretty(&run.to_dict())?);
    Ok(())
}

fn profiles() -> BTreeMap<String, AgentDefinition> {
    let mut researcher = AgentDefinition::default_for_model("kimi-k2.6");
    researcher.description = "你是研究助理, 先检索材料再输出结构化结论.".to_string();
    researcher.backend = Some("moonshot".to_string());
    researcher.max_cycles = 12;
    researcher.enable_todo_management = true;

    let mut translator = AgentDefinition::default_for_model("MiniMax-M2.5");
    translator.description = "你是专业翻译助理, 按段翻译并持续写入目标文件.".to_string();
    translator.backend = Some("minimax".to_string());
    translator.max_cycles = 20;
    translator.enable_todo_management = true;

    let mut computer = AgentDefinition::default_for_model("kimi-k2.6");
    computer.description = "你是桌面执行代理, 优先使用工具完成任务.".to_string();
    computer.backend = Some("moonshot".to_string());
    computer.max_cycles = 16;
    computer.agent_type = Some("computer".to_string());

    BTreeMap::from([
        ("researcher".to_string(), researcher),
        ("translator".to_string(), translator),
        ("computer".to_string(), computer),
    ])
}
