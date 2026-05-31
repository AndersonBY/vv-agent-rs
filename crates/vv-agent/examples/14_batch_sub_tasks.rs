#![allow(deprecated)]

use std::collections::BTreeMap;

mod common;

use common::{print_run, runtime_log_handler, ExampleConfig};
use vv_agent::{AgentDefinition, AgentSDKClient, AgentSDKOptions, SubAgentConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = ExampleConfig::load();
    config.workspace = config.workspace.join("batch_demo");
    config.ensure_workspace()?;
    let docs_dir = config.workspace.join("docs");
    std::fs::create_dir_all(&docs_dir)?;
    for (index, (title, body)) in [
        (
            "API Design",
            "RESTful API should use resource-oriented URLs and proper HTTP verbs.",
        ),
        (
            "Testing Strategy",
            "Unit tests cover logic; integration tests cover boundaries.",
        ),
        (
            "Deployment",
            "Blue-green deployment minimizes downtime during releases.",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        std::fs::write(
            docs_dir.join(format!("doc_{}.md", index + 1)),
            format!("# {title}\n\n{body}\n"),
        )?;
    }

    let mut agent = AgentDefinition::default_for_model(config.model.clone());
    agent.description = concat!(
        "你是文档处理总控 Agent. 使用 `create_sub_task` 的 `tasks` 批量模式",
        "并行分派多个文档摘要任务给子 Agent, 最后汇总结果写入 artifacts/summary.md。"
    )
    .to_string();
    agent.backend = Some(config.backend.clone());
    agent.max_cycles = 20;
    agent.enable_sub_agents = true;
    agent.enable_todo_management = true;
    agent.sub_agents = BTreeMap::from([
        (
            "summarizer-a".to_string(),
            SubAgentConfig {
                backend: Some(config.backend.clone()),
                max_cycles: 6,
                ..SubAgentConfig::new(config.model.clone(), "负责阅读单篇文档并输出中文摘要。")
            },
        ),
        (
            "summarizer-b".to_string(),
            SubAgentConfig {
                backend: Some(config.backend.clone()),
                max_cycles: 6,
                ..SubAgentConfig::new(config.model.clone(), "负责阅读单篇文档并输出中文摘要。")
            },
        ),
    ]);

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
    let run = client.run(
        "docs/ 目录下有 3 篇文档. 请使用 `create_sub_task` 的 `tasks` 批量模式并行分派给子 Agent, \
         每个子任务负责读取一篇文档并输出中文摘要. 所有子任务完成后, 写入 artifacts/summary.md, \
         然后调用 `task_finish` 输出结论。",
    )?;
    print_run(&run)
}
