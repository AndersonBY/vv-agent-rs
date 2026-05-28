use std::sync::Arc;

use serde_json::{json, Value};

mod common;

use common::{print_run, runtime_log_handler, ExampleConfig};
use vv_agent::constants::TASK_FINISH_TOOL_NAME;
use vv_agent::{
    build_default_registry, AgentDefinition, AgentSDKClient, AgentSDKOptions, ToolContext,
    ToolExecutionResult, ToolRegistry,
};

const TICKET_STORE_TOOL_NAME: &str = "_ticket_store";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let sample_log = config.workspace.join("logs/app.log");
    if !sample_log.exists() {
        std::fs::create_dir_all(sample_log.parent().expect("log parent"))?;
        std::fs::write(
            &sample_log,
            [
                "2026-02-18T01:21:03Z ERROR payment retry timeout order=ORD-1001",
                "2026-02-18T01:21:12Z WARN redis connection unstable",
                "2026-02-18T01:22:09Z ERROR email provider 503 campaign=SPRING",
                "2026-02-18T01:22:44Z ERROR search index lag exceeds threshold",
            ]
            .join("\n"),
        )?;
    }

    let mut agent = AgentDefinition::default_for_model(config.model.clone());
    agent.description = concat!(
        "你是 SRE 值班助理. 先从日志中提炼问题, 再调用自定义工单工具落盘,",
        "最后给出处理优先级建议。"
    )
    .to_string();
    agent.backend = Some(config.backend.clone());
    agent.max_cycles = 18;
    agent.enable_todo_management = true;
    agent.extra_tool_names = vec![TICKET_STORE_TOOL_NAME.to_string()];

    let client = AgentSDKClient::new_with_agent(
        AgentSDKOptions {
            settings_file: config.settings_file,
            default_backend: config.backend,
            workspace: config.workspace,
            tool_registry_factory: Some(Arc::new(build_registry_with_ticket_tool)),
            log_handler: runtime_log_handler(config.verbose),
            ..AgentSDKOptions::default()
        },
        agent,
    );
    let run = client.run(format!(
        concat!(
            "请读取 logs/app.log, 提炼至少 3 个问题并调用 `{}` action=create 写入工单。",
            "然后调用 `{}` action=list 检查当前工单。",
            "最后调用 `{}` 输出简洁结论。"
        ),
        TICKET_STORE_TOOL_NAME, TICKET_STORE_TOOL_NAME, TASK_FINISH_TOOL_NAME
    ))?;
    print_run(&run)
}

fn build_registry_with_ticket_tool() -> ToolRegistry {
    let mut registry = build_default_registry();
    registry
        .register_tool_with_parameters(
            TICKET_STORE_TOOL_NAME,
            "Manage local support tickets. Actions: create, list.",
            json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "description": "One of: create, list."},
                    "title": {"type": "string", "description": "Ticket title."},
                    "description": {"type": "string", "description": "Ticket description."},
                    "severity": {"type": "string", "description": "low/medium/high."}
                },
                "required": ["action"]
            }),
            Arc::new(ticket_store),
        )
        .expect("register ticket tool");
    registry
}

fn ticket_store(
    context: &mut ToolContext,
    arguments: &vv_agent::types::ToolArguments,
) -> ToolExecutionResult {
    let action = arguments
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let db_path = match context.resolve_workspace_path("artifacts/tickets.json") {
        Ok(path) => path,
        Err(error) => return ToolExecutionResult::error("", error),
    };
    let mut tickets = std::fs::read_to_string(&db_path)
        .ok()
        .and_then(|content| serde_json::from_str::<Vec<Value>>(&content).ok())
        .unwrap_or_default();
    match action.as_str() {
        "create" => {
            let title = arguments
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            if title.is_empty() {
                return ToolExecutionResult::error(
                    "",
                    json!({"ok": false, "error": "`title` is required"}).to_string(),
                );
            }
            let ticket = json!({
                "id": format!("T-{:08x}", tickets.len() + 1),
                "title": title,
                "description": arguments.get("description").and_then(Value::as_str).unwrap_or_default(),
                "severity": arguments.get("severity").and_then(Value::as_str).unwrap_or("medium"),
                "status": "open",
            });
            tickets.push(ticket.clone());
            if let Some(parent) = db_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&db_path, serde_json::to_string_pretty(&tickets).unwrap());
            ToolExecutionResult::success(
                "",
                json!({"ok": true, "action": action, "ticket": ticket, "total": tickets.len()})
                    .to_string(),
            )
        }
        "list" => ToolExecutionResult::success(
            "",
            json!({"ok": true, "action": action, "count": tickets.len(), "tickets": tickets})
                .to_string(),
        ),
        _ => ToolExecutionResult::error(
            "",
            json!({"ok": false, "error": "Invalid action. Use create/list."}).to_string(),
        ),
    }
}
