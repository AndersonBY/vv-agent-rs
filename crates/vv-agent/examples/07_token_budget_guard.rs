#![allow(deprecated)]

use serde_json::Value;

mod common;

use common::{env_u64, print_run, runtime_log_handler, ExampleConfig};
use vv_agent::constants::{task_finish_tool_schema, TASK_FINISH_TOOL_NAME};
use vv_agent::runtime::normalize_token_usage;
use vv_agent::{
    AfterLlmEvent, AgentDefinition, AgentSDKClient, AgentSDKOptions, BeforeLlmEvent,
    BeforeLlmPatch, LLMResponse, Message, RuntimeHook, ToolCall,
};

struct TokenBudgetHook {
    token_budget: u64,
    consumed_tokens: std::sync::Mutex<u64>,
    finalize_mode: std::sync::Mutex<bool>,
    finalize_prompt_injected: std::sync::Mutex<bool>,
    verbose: bool,
}

impl TokenBudgetHook {
    fn new(token_budget: u64, verbose: bool) -> Self {
        Self {
            token_budget: token_budget.max(1),
            consumed_tokens: std::sync::Mutex::new(0),
            finalize_mode: std::sync::Mutex::new(false),
            finalize_prompt_injected: std::sync::Mutex::new(false),
            verbose,
        }
    }
}

impl RuntimeHook for TokenBudgetHook {
    fn before_llm(&self, event: BeforeLlmEvent<'_>) -> Option<BeforeLlmPatch> {
        if !*self.finalize_mode.lock().expect("finalize mode lock") {
            return None;
        }
        let mut tool_schemas = event
            .tool_schemas
            .iter()
            .filter(|schema| {
                schema
                    .get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
                    == Some(TASK_FINISH_TOOL_NAME)
            })
            .cloned()
            .collect::<Vec<_>>();
        if tool_schemas.is_empty() {
            tool_schemas.push(task_finish_tool_schema());
        }
        let mut messages = event.messages.to_vec();
        let mut injected = self
            .finalize_prompt_injected
            .lock()
            .expect("finalize prompt lock");
        if !*injected {
            messages.push(Message::user(
                "Token budget 已达上限. 请基于现有信息给出最终简洁总结, 并调用 task_finish.",
            ));
            *injected = true;
        }
        Some(BeforeLlmPatch {
            messages: Some(messages),
            tool_schemas: Some(tool_schemas),
        })
    }

    fn after_llm(&self, event: AfterLlmEvent<'_>) -> Option<LLMResponse> {
        let usage = normalize_token_usage(event.response.raw.get("usage").unwrap_or(&Value::Null));
        let cycle_tokens = usage
            .total_tokens
            .max(usage.prompt_tokens + usage.completion_tokens);
        let mut consumed = self.consumed_tokens.lock().expect("consumed lock");
        *consumed += cycle_tokens;
        if self.verbose {
            eprintln!(
                "[hook.token_budget] cycle={} cycle_tokens={} total_tokens={}/{}",
                event.cycle_index, cycle_tokens, *consumed, self.token_budget
            );
        }
        let has_finish = event
            .response
            .tool_calls
            .iter()
            .any(|call| call.name == TASK_FINISH_TOOL_NAME);
        let mut finalize_mode = self.finalize_mode.lock().expect("finalize mode lock");
        if *finalize_mode {
            if has_finish {
                return None;
            }
            let summary = if event.response.content.trim().is_empty() {
                "Token budget reached. Please run another task if you need deeper analysis."
            } else {
                event.response.content.trim()
            };
            let mut response = event.response.clone();
            response.tool_calls = vec![ToolCall::from_raw_arguments(
                format!("budget_finish_{}", event.cycle_index),
                TASK_FINISH_TOOL_NAME,
                serde_json::json!({"message": summary}),
            )];
            return Some(response);
        }
        if *consumed < self.token_budget || has_finish {
            return None;
        }
        *finalize_mode = true;
        let mut response = event.response.clone();
        response.tool_calls.clear();
        Some(response)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ExampleConfig::load();
    config.ensure_workspace()?;
    let token_budget = env_u64("V_AGENT_EXAMPLE_TOKEN_BUDGET", 6000);

    let mut agent = AgentDefinition::default_for_model(config.model.clone());
    agent.description = "你是迭代式执行 Agent. 先探索问题, 再给出可执行方案.".to_string();
    agent.backend = Some(config.backend.clone());
    agent.max_cycles = 24;
    agent.enable_todo_management = true;

    let client = AgentSDKClient::new_with_agent(
        AgentSDKOptions {
            settings_file: config.settings_file,
            default_backend: config.backend,
            workspace: config.workspace,
            runtime_hooks: vec![std::sync::Arc::new(TokenBudgetHook::new(
                token_budget,
                config.verbose,
            ))],
            log_handler: runtime_log_handler(config.verbose),
            ..AgentSDKOptions::default()
        },
        agent,
    );
    let run = client.run("请梳理 workspace 下的任务上下文, 形成一个可执行计划.")?;
    print_run(&run)
}
