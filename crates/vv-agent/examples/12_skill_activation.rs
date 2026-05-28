mod common;

use common::{env_string, env_u32, print_run, runtime_log_handler, ExampleConfig};
use vv_agent::{AgentDefinition, AgentSDKClient, AgentSDKOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let skills_dir = env_string("V_AGENT_EXAMPLE_SKILLS_DIR", "skills");
    let max_cycles = env_u32("V_AGENT_EXAMPLE_MAX_CYCLES", 80).max(1);
    let source_root = {
        let path = std::path::PathBuf::from(&skills_dir);
        if path.is_absolute() {
            path
        } else {
            config.workspace.join(path)
        }
    };
    if !source_root.is_dir() {
        return Err(format!("Skills directory not found: {}", source_root.display()).into());
    }
    let skills_dir_for_agent = source_root
        .strip_prefix(&config.workspace)
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| source_root.display().to_string());
    eprintln!("[skills] using skill directory: {skills_dir_for_agent}");

    let mut agent = AgentDefinition::default_for_model(config.model.clone());
    agent.description =
        "你是 Remotion 视频工程助手, 会自主匹配并激活合适技能后落地代码.".to_string();
    agent.backend = Some(config.backend.clone());
    agent.language = "zh-CN".to_string();
    agent.max_cycles = max_cycles;
    agent.enable_todo_management = true;
    agent.use_workspace = true;
    agent.agent_type = Some("computer".to_string());
    agent.skill_directories = vec![skills_dir_for_agent];

    let prompt = concat!(
        "请执行一个 Remotion 视频工程任务, 并尽量利用已提供的技能元数据:\n",
        "1) 先查看 `<available_skills>` 列表, 若有匹配技能, 自主调用 `activate_skill`.\n",
        "2) 激活后读取必要规则文件并记录读取了哪些文件.\n",
        "3) 在 `artifacts/remotion_demo/` 下生成最小 Remotion 工程骨架.\n",
        "4) 写出 `artifacts/remotion_demo/README_zh.md`.\n",
        "5) 最后调用 `task_finish`, 汇报激活技能名、读取规则文件、生成路径、下一步命令.\n"
    );

    let client = AgentSDKClient::new_with_agent(
        AgentSDKOptions {
            settings_file: config.settings_file,
            default_backend: config.backend,
            workspace: config.workspace,
            log_handler: runtime_log_handler(config.verbose),
            ..AgentSDKOptions::default()
        },
        agent,
    );
    let run = client.run(prompt)?;
    print_run(&run)
}
