use std::collections::BTreeMap;

use serde_json::json;
use vv_agent::{
    AgentRuntime, AgentStatus, AgentTask, LLMResponse, ScriptedLlmClient, ToolCall, ToolDirective,
};

#[test]
fn runtime_executes_tool_calls_until_task_finish() {
    let mut finish_args = BTreeMap::new();
    finish_args.insert(
        "message".to_string(),
        json!("final answer from task_finish"),
    );
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "",
        vec![ToolCall::new("call_1", "task_finish", finish_args)],
    )]);
    let runtime = AgentRuntime::new(llm);

    let result = runtime
        .run(AgentTask::new("task_1", "demo", "system", "finish now"))
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.final_answer.as_deref(),
        Some("final answer from task_finish")
    );
    assert_eq!(result.cycles.len(), 1);
    assert_eq!(result.cycles[0].tool_results.len(), 1);
    assert_eq!(
        result.cycles[0].tool_results[0].directive,
        ToolDirective::Finish
    );
    assert_eq!(
        result.messages.last().unwrap().tool_call_id.as_deref(),
        Some("call_1")
    );
}

#[test]
fn runtime_waits_when_ask_user_tool_requests_input() {
    let mut ask_args = BTreeMap::new();
    ask_args.insert("question".to_string(), json!("Which option should I use?"));
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "",
        vec![ToolCall::new("call_1", "ask_user", ask_args)],
    )]);
    let runtime = AgentRuntime::new(llm);

    let result = runtime
        .run(AgentTask::new("task_1", "demo", "system", "ask"))
        .expect("run");

    assert_eq!(result.status, AgentStatus::WaitUser);
    assert_eq!(
        result.wait_reason.as_deref(),
        Some("Which option should I use?")
    );
}
