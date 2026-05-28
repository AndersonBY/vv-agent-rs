use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;
use vv_agent::{
    ToolCall, ToolCallRunner, ToolContext, ToolDirective, ToolExecutionResult, ToolRegistry,
    ToolResultStatus, ToolRunRequest, ToolSpec,
};

#[test]
fn tool_call_runner_skips_remaining_calls_after_finish() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut registry = ToolRegistry::new();
    registry
        .register(ToolSpec::new(
            "_finish",
            "finish",
            Arc::new(|_context, _arguments| {
                let mut result = ToolExecutionResult::success("", json!({"ok": true}).to_string());
                result.directive = ToolDirective::Finish;
                result
                    .metadata
                    .insert("final_message".to_string(), json!("finished"));
                result
            }),
        ))
        .expect("finish tool");
    registry
        .register(ToolSpec::new(
            "_never",
            "must not run",
            Arc::new(|_context, _arguments| ToolExecutionResult::error("", "should not run")),
        ))
        .expect("never tool");
    let runner = ToolCallRunner::new(registry);
    let task = vv_agent::AgentTask::new("tool_runner", "demo", "system", "prompt");
    let mut context = ToolContext::new(workspace.path());
    let tool_calls = vec![
        ToolCall::new("finish_call", "_finish", BTreeMap::new()),
        ToolCall::new("skipped_call", "_never", BTreeMap::new()),
    ];
    let mut messages = Vec::new();
    let mut cycle = vv_agent::CycleRecord {
        index: 1,
        assistant_message: String::new(),
        tool_calls: tool_calls.clone(),
        tool_results: Vec::new(),
        memory_compacted: false,
        token_usage: vv_agent::TokenUsage::default(),
    };

    let outcome = runner
        .run(ToolRunRequest::new(
            &task,
            tool_calls,
            &mut context,
            &mut messages,
            &mut cycle,
        ))
        .expect("tool run");

    assert_eq!(
        outcome.directive_result.expect("directive").directive,
        ToolDirective::Finish
    );
    assert_eq!(cycle.tool_results.len(), 2);
    assert_eq!(cycle.tool_results[0].tool_call_id, "finish_call");
    assert_eq!(cycle.tool_results[1].tool_call_id, "skipped_call");
    assert_eq!(
        cycle.tool_results[1].error_code.as_deref(),
        Some("skipped_due_to_finish")
    );
    assert_eq!(cycle.tool_results[1].status, ToolResultStatus::Error);
    assert_eq!(messages.len(), 2);
}
