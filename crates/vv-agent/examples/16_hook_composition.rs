use std::sync::Mutex;
use std::time::Instant;

use serde_json::{json, Value};

mod common;

use common::{print_run, runtime_log_handler, ExampleConfig};
use vv_agent::runtime::normalize_token_usage;
use vv_agent::{
    AfterLlmEvent, AfterToolCallEvent, AgentDefinition, AgentSDKClient, AgentSDKOptions,
    BeforeLlmEvent, BeforeToolCallEvent, RuntimeHook, ToolDirective, ToolExecutionResult,
    ToolResultStatus,
};

struct TimingHook {
    cycle_start: Mutex<Option<Instant>>,
}

impl RuntimeHook for TimingHook {
    fn before_llm(&self, _event: BeforeLlmEvent<'_>) -> Option<vv_agent::BeforeLlmPatch> {
        *self.cycle_start.lock().expect("timing lock") = Some(Instant::now());
        None
    }

    fn after_llm(&self, event: AfterLlmEvent<'_>) -> Option<vv_agent::LLMResponse> {
        let elapsed = self
            .cycle_start
            .lock()
            .expect("timing lock")
            .map(|start| start.elapsed().as_secs_f32())
            .unwrap_or_default();
        let usage = normalize_token_usage(event.response.raw.get("usage").unwrap_or(&Value::Null));
        eprintln!(
            "[TimingHook] cycle={} latency={elapsed:.2}s tokens={}",
            event.cycle_index, usage.total_tokens
        );
        None
    }
}

struct SafetyHook;

impl RuntimeHook for SafetyHook {
    fn before_tool_call(
        &self,
        event: BeforeToolCallEvent<'_>,
    ) -> Option<vv_agent::BeforeToolCallPatch> {
        let path = event
            .call
            .arguments
            .get("path")
            .or_else(|| event.call.arguments.get("file_path"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        if [".env", "credentials", "secret"]
            .iter()
            .any(|pattern| path.contains(pattern))
        {
            eprintln!("[SafetyHook] BLOCKED tool={} path={path}", event.call.name);
            return Some(
                ToolExecutionResult {
                    tool_call_id: event.call.id.clone(),
                    status: ToolResultStatus::Error,
                    directive: ToolDirective::Continue,
                    error_code: Some("blocked_by_safety_hook".to_string()),
                    content: json!({"ok": false, "error": "blocked by safety policy"}).to_string(),
                    metadata: Default::default(),
                    image_url: None,
                    image_path: None,
                }
                .into(),
            );
        }
        None
    }
}

struct AuditHook {
    tool_calls: Mutex<Vec<Value>>,
}

impl RuntimeHook for AuditHook {
    fn after_tool_call(&self, event: AfterToolCallEvent<'_>) -> Option<ToolExecutionResult> {
        let entry = json!({
            "cycle": event.cycle_index,
            "tool": event.call.name,
            "status": format!("{:?}", event.result.status),
        });
        self.tool_calls
            .lock()
            .expect("audit lock")
            .push(entry.clone());
        eprintln!("[AuditHook] {entry}");
        None
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let audit = std::sync::Arc::new(AuditHook {
        tool_calls: Mutex::new(Vec::new()),
    });

    let mut agent = AgentDefinition::default_for_model(config.model.clone());
    agent.description = "你是文件处理 Agent. 读取 workspace 下的文件并输出摘要.".to_string();
    agent.backend = Some(config.backend.clone());
    agent.max_cycles = 12;
    agent.enable_todo_management = true;

    let client = AgentSDKClient::new_with_agent(
        AgentSDKOptions {
            settings_file: config.settings_file,
            default_backend: config.backend,
            workspace: config.workspace,
            runtime_hooks: vec![
                std::sync::Arc::new(TimingHook {
                    cycle_start: Mutex::new(None),
                }),
                std::sync::Arc::new(SafetyHook),
                audit.clone(),
            ],
            log_handler: runtime_log_handler(config.verbose),
            ..AgentSDKOptions::default()
        },
        agent,
    );
    let run = client.run(
        "请读取 workspace 下所有文件并输出摘要. 注意: 不要尝试读取 .env 或 credentials 相关文件. \
         完成后调用 `task_finish` 输出结论。",
    )?;
    print_run(&run)?;
    println!(
        "\n[AuditHook] Total tool calls recorded: {}",
        audit.tool_calls.lock().expect("audit lock").len()
    );
    Ok(())
}
