mod common;

use std::sync::Arc;

use common::{print_run_result, run_facade_prompt, ExampleConfig};
use vv_agent::{BeforeMemoryCompactEvent, Message, RunConfig, RuntimeHook};

struct KeepProjectNameHook;

impl RuntimeHook for KeepProjectNameHook {
    fn before_memory_compact(&self, event: BeforeMemoryCompactEvent<'_>) -> Option<Vec<Message>> {
        let mut messages = event.messages.to_vec();
        messages.push(Message::system(
            "压缩记忆时保留项目名 vv-agent-rs 和当前任务目标。",
        ));
        Some(messages)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    let result = run_facade_prompt(
        &config,
        "memory-demo",
        "你会产生多轮中间推理，并在上下文变长时保持核心任务信息。",
        "连续列出项目风险、机会、验证步骤，最后汇总。",
        RunConfig::builder()
            .hook(Arc::new(KeepProjectNameHook))
            .max_cycles(10)
            .build(),
    )
    .await?;
    print_run_result(&result)
}
