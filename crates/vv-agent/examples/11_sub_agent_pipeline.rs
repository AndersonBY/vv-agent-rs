#![allow(deprecated)]

use std::collections::BTreeMap;

mod common;

use common::{print_run, runtime_log_handler, ExampleConfig};
use vv_agent::{AgentDefinition, AgentSDKClient, AgentSDKOptions, SubAgentConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let input_dir = config.workspace.join("inputs");
    std::fs::create_dir_all(&input_dir)?;
    std::fs::write(
        input_dir.join("product_brief.md"),
        "# Product Brief\n\n- Product: VectorVein Agent Platform\n- Goal: Build reliable multi-agent runtime.\n- KPI: Reduce failed runs by 35%.\n",
    )?;
    std::fs::write(
        input_dir.join("ops_notes.md"),
        "# Ops Notes\n\n- Main risk: models may loop without clear finish signal.\n- Priority: improve observability and guardrails.\n",
    )?;

    let mut agent = AgentDefinition::default_for_model(config.model.clone());
    agent.description = concat!(
        "你是项目总控 Agent. 你必须优先将任务委派给子 Agent, 再整合产出最终结果.",
        "最终请把报告写入 artifacts/final_report.md。"
    )
    .to_string();
    agent.backend = Some(config.backend.clone());
    agent.max_cycles = 20;
    agent.enable_sub_agents = true;
    agent.enable_todo_management = true;
    agent.sub_agents = BTreeMap::from([
        (
            "research-sub".to_string(),
            SubAgentConfig {
                backend: Some(config.backend.clone()),
                max_cycles: 8,
                ..SubAgentConfig::new(config.model.clone(), "负责阅读输入文档并提取事实要点。")
            },
        ),
        (
            "writer-sub".to_string(),
            SubAgentConfig {
                backend: Some(config.backend.clone()),
                max_cycles: 10,
                ..SubAgentConfig::new(config.model.clone(), "负责把要点写成中文可执行报告。")
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
        "请先调用 `create_sub_task` 给 `research-sub`, 读取 inputs/ 下文档并产出结构化要点. \
         然后调用 `create_sub_task` 给 `writer-sub`, 基于要点输出 `artifacts/final_report.md` 的正文草稿. \
         最后由你整合并确认结果。",
    )?;
    print_run(&run)
}
