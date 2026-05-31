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

struct SimpleBudgetHook {
    budget: u64,
    total_tokens: std::sync::Mutex<u64>,
    finalize_mode: std::sync::Mutex<bool>,
    finalize_injected: std::sync::Mutex<bool>,
}

impl SimpleBudgetHook {
    fn new(budget: u64) -> Self {
        Self {
            budget,
            total_tokens: std::sync::Mutex::new(0),
            finalize_mode: std::sync::Mutex::new(false),
            finalize_injected: std::sync::Mutex::new(false),
        }
    }
}

impl RuntimeHook for SimpleBudgetHook {
    fn after_llm(&self, event: AfterLlmEvent<'_>) -> Option<LLMResponse> {
        let usage = normalize_token_usage(event.response.raw.get("usage").unwrap_or(&Value::Null));
        let tokens = usage
            .total_tokens
            .max(usage.prompt_tokens + usage.completion_tokens);
        let mut total = self.total_tokens.lock().expect("total token lock");
        *total += tokens;
        let mut finalize = self.finalize_mode.lock().expect("finalize mode lock");
        if *finalize {
            if event
                .response
                .tool_calls
                .iter()
                .any(|call| call.name == TASK_FINISH_TOOL_NAME)
            {
                return None;
            }
            let mut response = event.response.clone();
            response.tool_calls = vec![ToolCall::from_raw_arguments(
                "budget_finish",
                TASK_FINISH_TOOL_NAME,
                serde_json::json!({"message": event.response.content.trim()}),
            )];
            return Some(response);
        }
        if *total >= self.budget {
            *finalize = true;
            let mut response = event.response.clone();
            response.tool_calls.clear();
            return Some(response);
        }
        None
    }

    fn before_llm(&self, event: BeforeLlmEvent<'_>) -> Option<BeforeLlmPatch> {
        if !*self.finalize_mode.lock().expect("finalize mode lock") {
            return None;
        }
        let mut injected = self
            .finalize_injected
            .lock()
            .expect("finalize injected lock");
        if *injected {
            return None;
        }
        *injected = true;
        let restricted = event
            .tool_schemas
            .iter()
            .find(|schema| {
                schema
                    .get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
                    == Some(TASK_FINISH_TOOL_NAME)
            })
            .cloned()
            .unwrap_or_else(task_finish_tool_schema);
        let mut messages = event.messages.to_vec();
        messages.push(Message::user(
            "Token budget 已达上限. 请立即调用 task_finish 给出简洁总结.",
        ));
        Some(BeforeLlmPatch {
            messages: Some(messages),
            tool_schemas: Some(vec![restricted]),
        })
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = ExampleConfig::load();
    config.workspace = config.workspace.join("arxiv_memory_demo");
    config.ensure_workspace()?;
    let token_budget = env_u64("V_AGENT_EXAMPLE_TOKEN_BUDGET", 50000);

    let prompt = concat!(
        "请完成一个端到端任务, 主题是 AI Agent Memory.\n",
        "1) 在 arXiv 搜索最近 30 天内发布且与 AI Agent Memory 高度相关的论文, 选择 1 篇最匹配的.\n",
        "2) 将 PDF 下载到 `artifacts/paper.pdf`, 元数据保存到 `artifacts/paper_meta.json`.\n",
        "3) 提取第一张图片为 `artifacts/figure1.png`.\n",
        "4) 必须调用 `read_image` 读取该图片并解释.\n",
        "5) 将论文内容翻译为中文并输出到 `artifacts/paper_zh.md`.\n",
        "6) 调用 `task_finish` 汇报文件路径和完成度.\n"
    );

    let mut agent = AgentDefinition::default_for_model(config.model.clone());
    agent.description =
        "你是资深 AI 研究助理, 擅长检索论文、处理 PDF、解释图片并做中文学术翻译.".to_string();
    agent.backend = Some(config.backend.clone());
    agent.language = "zh-CN".to_string();
    agent.max_cycles = 80;
    agent.enable_todo_management = true;
    agent.use_workspace = true;
    agent.agent_type = Some("computer".to_string());
    agent.native_multimodal = true;

    let client = AgentSDKClient::new_with_agent(
        AgentSDKOptions {
            settings_file: config.settings_file,
            default_backend: config.backend,
            workspace: config.workspace,
            log_handler: runtime_log_handler(config.verbose),
            runtime_hooks: vec![std::sync::Arc::new(SimpleBudgetHook::new(token_budget))],
            ..AgentSDKOptions::default()
        },
        agent,
    );
    let run = client.run(prompt)?;
    print_run(&run)
}
