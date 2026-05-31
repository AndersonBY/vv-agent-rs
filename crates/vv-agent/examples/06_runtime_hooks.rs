mod common;

use std::sync::Arc;

use common::{print_run_result, run_facade_prompt, ExampleConfig};
use vv_agent::{BeforeLlmEvent, BeforeLlmPatch, Message, RunConfig, RuntimeHook};

struct SystemReminderHook;

impl RuntimeHook for SystemReminderHook {
    fn before_llm(&self, event: BeforeLlmEvent<'_>) -> Option<BeforeLlmPatch> {
        let mut messages = event.messages.to_vec();
        messages.insert(0, Message::system("优先给出可执行步骤，避免泛泛而谈。"));
        Some(BeforeLlmPatch {
            messages: Some(messages),
            tool_schemas: None,
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    let result = run_facade_prompt(
        &config,
        "hooked-agent",
        "你是开发助手。按步骤完成任务并调用 task_finish。",
        "检查 workspace 并给出下一步建议。",
        RunConfig::builder()
            .hook(Arc::new(SystemReminderHook))
            .max_cycles(8)
            .build(),
    )
    .await?;
    print_run_result(&result)
}
