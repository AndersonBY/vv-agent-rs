#![allow(deprecated)]

mod common;

use common::{env_string, print_run, runtime_log_handler, ExampleConfig};
use vv_agent::{
    AgentDefinition, AgentSDKClient, AgentSDKOptions, BeforeMemoryCompactEvent, Message,
    RuntimeHook,
};

struct MemoryAuditHook {
    pin_keywords: Vec<String>,
    compact_count: std::sync::Mutex<u32>,
}

impl RuntimeHook for MemoryAuditHook {
    fn before_memory_compact(&self, event: BeforeMemoryCompactEvent<'_>) -> Option<Vec<Message>> {
        let mut count = self.compact_count.lock().expect("compact count lock");
        *count += 1;
        let total_chars: usize = event
            .messages
            .iter()
            .map(|message| message.content.len())
            .sum();
        eprintln!(
            "[MemoryAuditHook] compact #{}: {} messages, {} chars, cycle={}",
            *count,
            event.messages.len(),
            total_chars,
            event.cycle_index
        );
        if self.pin_keywords.is_empty() {
            return None;
        }
        let pinned = event
            .messages
            .iter()
            .filter(|message| {
                let content = message.content.to_ascii_lowercase();
                self.pin_keywords
                    .iter()
                    .any(|keyword| content.contains(keyword))
            })
            .cloned()
            .collect::<Vec<_>>();
        (!pinned.is_empty()).then_some(pinned)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let pin_keywords = env_string("V_AGENT_EXAMPLE_PIN_KEYWORDS", "priority,critical")
        .split(',')
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();

    let mut agent = AgentDefinition::default_for_model(config.model.clone());
    agent.description =
        "你是迭代执行 Agent. 每轮产出大量中间文本以触发 memory compaction.".to_string();
    agent.backend = Some(config.backend.clone());
    agent.max_cycles = 30;
    agent.enable_todo_management = true;

    let client = AgentSDKClient::new_with_agent(
        AgentSDKOptions {
            settings_file: config.settings_file,
            default_backend: config.backend,
            workspace: config.workspace,
            runtime_hooks: vec![std::sync::Arc::new(MemoryAuditHook {
                pin_keywords,
                compact_count: std::sync::Mutex::new(0),
            })],
            log_handler: runtime_log_handler(config.verbose),
            ..AgentSDKOptions::default()
        },
        agent,
    );
    let run = client.run(
        "请逐步生成一份详细的技术方案文档, 包含背景、目标、方案设计、风险评估、实施计划等章节. \
         标记 priority 和 critical 的内容会在 memory compaction 时被保留. 完成后调用 `task_finish`。",
    )?;
    print_run(&run)
}
