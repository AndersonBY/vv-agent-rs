#![allow(deprecated)]

use serde_json::{json, Value};

mod common;

use common::{print_run, runtime_log_handler, ExampleConfig};
use vv_agent::constants::WRITE_FILE_TOOL_NAME;
use vv_agent::{
    AgentDefinition, AgentSDKClient, AgentSDKOptions, BeforeLlmEvent, BeforeLlmPatch,
    BeforeToolCallEvent, Message, RuntimeHook, ToolDirective, ToolExecutionResult,
    ToolResultStatus,
};

struct GuardAndHintHook {
    verbose: bool,
}

impl RuntimeHook for GuardAndHintHook {
    fn before_llm(&self, event: BeforeLlmEvent<'_>) -> Option<BeforeLlmPatch> {
        if self.verbose {
            eprintln!(
                "[hook.before_llm] cycle={} messages={}",
                event.cycle_index,
                event.messages.len()
            );
        }
        let mut messages = event.messages.to_vec();
        messages.push(Message::user(
            "系统补充要求: 任何输出都要简洁, 并在结尾附上下一步建议.",
        ));
        Some(BeforeLlmPatch {
            messages: Some(messages),
            tool_schemas: None,
        })
    }

    fn before_tool_call(
        &self,
        event: BeforeToolCallEvent<'_>,
    ) -> Option<vv_agent::BeforeToolCallPatch> {
        if self.verbose {
            eprintln!(
                "[hook.before_tool_call] cycle={} tool={}",
                event.cycle_index, event.call.name
            );
        }
        if event.call.name != WRITE_FILE_TOOL_NAME {
            return None;
        }
        let path = event
            .call
            .arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !path.ends_with(".env") {
            return None;
        }
        Some(
            ToolExecutionResult {
                tool_call_id: event.call.id.clone(),
                status: ToolResultStatus::Error,
                directive: ToolDirective::Continue,
                error_code: Some("blocked_sensitive_path".to_string()),
                content: json!({"ok": false, "error": "Refuse writing .env from runtime hook"})
                    .to_string(),
                metadata: Default::default(),
                image_url: None,
                image_path: None,
            }
            .into(),
        )
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;

    let mut agent = AgentDefinition::default_for_model(config.model.clone());
    agent.description = "你是一个注重安全和可执行性的开发 Agent。".to_string();
    agent.backend = Some(config.backend.clone());
    agent.max_cycles = 20;
    agent.enable_todo_management = true;
    agent.use_workspace = true;

    let client = AgentSDKClient::new_with_agent(
        AgentSDKOptions {
            settings_file: config.settings_file,
            default_backend: config.backend,
            workspace: config.workspace,
            runtime_hooks: vec![std::sync::Arc::new(GuardAndHintHook {
                verbose: config.verbose,
            })],
            log_handler: runtime_log_handler(config.verbose),
            ..AgentSDKOptions::default()
        },
        agent,
    );
    let run = client
        .run("请尝试把 `TEST=1` 写到 .env, 然后再把最终结论写入 artifacts/hook_result.md.")?;
    print_run(&run)
}
