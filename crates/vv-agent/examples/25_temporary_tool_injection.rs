use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

mod common;

use common::{env_u32, print_run, runtime_log_handler, ExampleConfig};
use vv_agent::constants::TASK_FINISH_TOOL_NAME;
use vv_agent::{
    build_default_registry, AgentDefinition, AgentSDKClient, AgentSDKOptions, BeforeLlmEvent,
    BeforeLlmPatch, BeforeToolCallEvent, Message, RuntimeHook, ToolContext, ToolDirective,
    ToolExecutionResult, ToolRegistry, ToolResultStatus,
};

const EPHEMERAL_NOTE_TOOL_NAME: &str = "_ephemeral_note";

struct TemporaryToolWindowHook {
    start_cycle: u32,
    end_cycle: u32,
    min_finish_cycle: u32,
    last_signature: Mutex<Option<String>>,
    verbose: bool,
}

impl RuntimeHook for TemporaryToolWindowHook {
    fn before_llm(&self, event: BeforeLlmEvent<'_>) -> Option<BeforeLlmPatch> {
        let in_window = self.start_cycle <= event.cycle_index && event.cycle_index < self.end_cycle;
        let mut tool_schemas = event.tool_schemas.to_vec();
        if in_window && !tool_names(&tool_schemas).contains(&EPHEMERAL_NOTE_TOOL_NAME.to_string()) {
            tool_schemas.push(ephemeral_note_schema());
        }
        let signature = schema_signature(&tool_schemas);
        let mut last = self.last_signature.lock().expect("signature lock");
        if let Some(previous) = last.as_ref() {
            if previous != &signature {
                eprintln!(
                    "[hook.temp_tool] WARNING: tool schema signature changed {previous} -> {signature}. This can break LLM prompt cache."
                );
            }
        }
        *last = Some(signature.clone());
        if self.verbose {
            eprintln!(
                "[hook.temp_tool] cycle={} window={} tools={} signature={}",
                event.cycle_index,
                if in_window { "on" } else { "off" },
                tool_names(&tool_schemas).len(),
                signature
            );
        }
        if !in_window {
            return None;
        }
        let mut messages = event.messages.to_vec();
        messages.push(Message::user(format!(
            "系统提示: 本轮临时开放 `{EPHEMERAL_NOTE_TOOL_NAME}` 工具用于演示。动态增删 tools 会影响 prompt cache。"
        )));
        Some(BeforeLlmPatch {
            messages: Some(messages),
            tool_schemas: Some(tool_schemas),
        })
    }

    fn before_tool_call(
        &self,
        event: BeforeToolCallEvent<'_>,
    ) -> Option<vv_agent::BeforeToolCallPatch> {
        if event.call.name != TASK_FINISH_TOOL_NAME || event.cycle_index >= self.min_finish_cycle {
            return None;
        }
        Some(
            ToolExecutionResult {
                tool_call_id: event.call.id.clone(),
                status: ToolResultStatus::Error,
                directive: ToolDirective::Continue,
                error_code: Some("demo_force_more_cycles".to_string()),
                content: json!({
                    "ok": false,
                    "error": "Demo guard: task_finish is temporarily blocked so you can observe tool schema changes across cycles.",
                    "min_finish_cycle": self.min_finish_cycle,
                })
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
    let mut config = ExampleConfig::load();
    config.workspace = config.workspace.join("temp_tool_demo");
    config.ensure_workspace()?;
    let start_cycle = env_u32("V_AGENT_TEMP_TOOL_START_CYCLE", 2).max(1);
    let end_cycle = env_u32("V_AGENT_TEMP_TOOL_END_CYCLE", 4).max(start_cycle + 1);
    let min_finish_cycle = env_u32("V_AGENT_TEMP_TOOL_MIN_FINISH_CYCLE", 5).max(end_cycle);
    let max_cycles = env_u32("V_AGENT_EXAMPLE_MAX_CYCLES", 7);
    let context_file = config.workspace.join("input/context.md");
    std::fs::create_dir_all(context_file.parent().expect("context parent"))?;
    if !context_file.exists() {
        std::fs::write(
            &context_file,
            "# Demo Context\n\n- The goal is to demonstrate temporary tool injection.\n- Mention prompt-cache impact explicitly.\n",
        )?;
    }

    eprintln!(
        "[example.warning] This demo intentionally mutates tool schemas between cycles. This may break prompt-cache reuse."
    );
    eprintln!(
        "[example.config] temp_tool_window=[{start_cycle}, {end_cycle}) min_finish_cycle={min_finish_cycle} max_cycles={max_cycles}"
    );

    let mut agent = AgentDefinition::default_for_model(config.model.clone());
    agent.description = concat!(
        "你是运行时策略演示 Agent。你会按步骤读取上下文、在可用时使用临时工具、",
        "并最终总结风险与建议。"
    )
    .to_string();
    agent.backend = Some(config.backend.clone());
    agent.max_cycles = max_cycles;
    agent.allow_interruption = false;
    agent.enable_todo_management = true;
    agent.use_workspace = true;

    let client = AgentSDKClient::new_with_agent(
        AgentSDKOptions {
            settings_file: config.settings_file,
            default_backend: config.backend,
            workspace: config.workspace,
            tool_registry_factory: Some(Arc::new(build_registry_with_ephemeral_tool)),
            runtime_hooks: vec![Arc::new(TemporaryToolWindowHook {
                start_cycle,
                end_cycle,
                min_finish_cycle,
                last_signature: Mutex::new(None),
                verbose: config.verbose,
            })],
            log_handler: runtime_log_handler(config.verbose),
            ..AgentSDKOptions::default()
        },
        agent,
    );
    let run = client.run(format!(
        "请先读取 input/context.md 并给出执行计划。当你看到 `{EPHEMERAL_NOTE_TOOL_NAME}` 可用时, 调用一次写入简短 note。\
         最终请调用 `{TASK_FINISH_TOOL_NAME}`, 并说明动态增删 tools 对 prompt cache 的影响。"
    ))?;
    print_run(&run)
}

fn build_registry_with_ephemeral_tool() -> ToolRegistry {
    let mut registry = build_default_registry();
    registry
        .register_tool_with_parameters(
            EPHEMERAL_NOTE_TOOL_NAME,
            "Write one demo note to artifacts/ephemeral_notes.log.",
            json!({
                "type": "object",
                "properties": {
                    "note": {"type": "string", "description": "One short note line to append."}
                },
                "required": ["note"]
            }),
            Arc::new(ephemeral_note),
        )
        .expect("register ephemeral note tool");
    registry
}

fn ephemeral_note(
    context: &mut ToolContext,
    arguments: &vv_agent::types::ToolArguments,
) -> ToolExecutionResult {
    let note = arguments
        .get("note")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    if note.is_empty() {
        return ToolExecutionResult::error(
            "",
            json!({"ok": false, "error": "`note` is required"}).to_string(),
        );
    }
    let output_path = match context.resolve_workspace_path("artifacts/ephemeral_notes.log") {
        Ok(path) => path,
        Err(error) => return ToolExecutionResult::error("", error),
    };
    if let Some(parent) = output_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&output_path)
        .and_then(|mut file| {
            use std::io::Write;
            writeln!(file, "{note}")
        });
    ToolExecutionResult::success(
        "",
        json!({"ok": true, "tool": EPHEMERAL_NOTE_TOOL_NAME, "note": note, "path": "artifacts/ephemeral_notes.log"}).to_string(),
    )
}

fn ephemeral_note_schema() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": EPHEMERAL_NOTE_TOOL_NAME,
            "description": "Write one demo note to artifacts/ephemeral_notes.log.",
            "parameters": {
                "type": "object",
                "properties": {
                    "note": {"type": "string", "description": "One short note line to append."}
                },
                "required": ["note"]
            }
        }
    })
}

fn tool_names(tool_schemas: &[Value]) -> Vec<String> {
    tool_schemas
        .iter()
        .filter_map(|schema| {
            schema
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

fn schema_signature(tool_schemas: &[Value]) -> String {
    let mut names = tool_names(tool_schemas);
    names.sort();
    let raw = names.join(",");
    let digest = Sha256::digest(raw.as_bytes());
    format!("{:x}", digest)[..12].to_string()
}
