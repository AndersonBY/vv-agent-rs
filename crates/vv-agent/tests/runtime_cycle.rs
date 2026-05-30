use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::{
    memory::CLEARED_MARKER, AgentRuntime, AgentStatus, AgentTask, BeforeLlmPatch,
    BeforeToolCallPatch, CancellationToken, ExecutionContext, LLMResponse, LlmClient, LlmError,
    LlmRequest, LlmStreamCallback, Message, RuntimeHook, RuntimeRunControls, ScriptedLlmClient,
    SubAgentConfig, TokenUsage, ToolCall, ToolDirective, ToolExecutionResult,
};

const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x60, 0x00, 0x00, 0x00,
    0x02, 0x00, 0x01, 0xe2, 0x21, 0xbc, 0x33, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

fn preview_text_for_test(text: &str, log_preview_chars: Option<usize>) -> String {
    let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let Some(limit) = log_preview_chars.map(|limit| limit.max(40)) else {
        return cleaned;
    };
    if cleaned.chars().count() <= limit {
        return cleaned;
    }
    format!(
        "{}...",
        cleaned
            .chars()
            .take(limit.saturating_sub(3))
            .collect::<String>()
    )
}

#[path = "runtime_cycle/cancellation.rs"]
mod cancellation;
#[path = "runtime_cycle/compaction.rs"]
mod compaction;
#[path = "runtime_cycle/core.rs"]
mod core;
#[path = "runtime_cycle/hooks.rs"]
mod hooks;
#[path = "runtime_cycle/microcompact.rs"]
mod microcompact;
#[path = "runtime_cycle/prompt_too_long.rs"]
mod prompt_too_long;
#[path = "runtime_cycle/session_memory.rs"]
mod session_memory;
#[path = "runtime_cycle/sub_agents.rs"]
mod sub_agents;
#[path = "runtime_cycle/sub_tasks.rs"]
mod sub_tasks;
