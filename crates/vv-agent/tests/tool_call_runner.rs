use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;
use vv_agent::{
    BeforeToolCallEvent, BeforeToolCallPatch, RuntimeHook, RuntimeHookManager, ToolCall,
    ToolCallRunner, ToolContext, ToolDirective, ToolExecutionResult, ToolRegistry,
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
    let skipped_payload: serde_json::Value =
        serde_json::from_str(&cycle.tool_results[1].content).expect("skipped payload json");
    assert_eq!(
        skipped_payload,
        json!({
            "ok": false,
            "error": "Tool skipped because a previous tool finished the task.",
            "error_code": "skipped_due_to_finish",
        })
    );
    let skipped_message_payload: serde_json::Value =
        serde_json::from_str(&messages[1].content).expect("skipped message payload json");
    assert_eq!(skipped_message_payload, skipped_payload);
    assert_eq!(messages.len(), 2);
}

#[test]
fn tool_call_runner_applies_stop_on_first_success_to_all_registered_tools() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut registry = ToolRegistry::new();
    for name in ["first", "second"] {
        registry
            .register(ToolSpec::new(
                name,
                name,
                Arc::new(|_context, _arguments| ToolExecutionResult::success("", "tool output")),
            ))
            .expect("tool");
    }
    let runner = ToolCallRunner::new(registry);
    let mut task = vv_agent::AgentTask::new("tool_runner", "demo", "system", "prompt");
    task.metadata.insert(
        "_vv_agent_tool_use_behavior".to_string(),
        json!("stop_on_first_tool"),
    );
    let mut context = ToolContext::new(workspace.path());
    let tool_calls = vec![
        ToolCall::new("first_call", "first", BTreeMap::new()),
        ToolCall::new("second_call", "second", BTreeMap::new()),
    ];
    let mut messages = Vec::new();
    let mut cycle = empty_cycle(tool_calls.clone());

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
    assert_eq!(cycle.tool_results[0].status, ToolResultStatus::Success);
    assert_eq!(
        cycle.tool_results[1].error_code.as_deref(),
        Some("skipped_due_to_finish")
    );
}

#[test]
fn tool_call_runner_stops_only_at_named_successful_tools() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut registry = ToolRegistry::new();
    for name in ["prepare", "publish"] {
        registry
            .register(ToolSpec::new(
                name,
                name,
                Arc::new(move |_context, _arguments| {
                    ToolExecutionResult::success("", format!("{name} output"))
                }),
            ))
            .expect("tool");
    }
    let runner = ToolCallRunner::new(registry);
    let mut task = vv_agent::AgentTask::new("tool_runner", "demo", "system", "prompt");
    task.metadata.insert(
        "_vv_agent_tool_use_behavior".to_string(),
        json!("stop_at_tool_names"),
    );
    task.metadata.insert(
        "_vv_agent_stop_at_tool_names".to_string(),
        json!(["publish"]),
    );
    let mut context = ToolContext::new(workspace.path());
    let tool_calls = vec![
        ToolCall::new("prepare_call", "prepare", BTreeMap::new()),
        ToolCall::new("publish_call", "publish", BTreeMap::new()),
    ];
    let mut messages = Vec::new();
    let mut cycle = empty_cycle(tool_calls.clone());

    let outcome = runner
        .run(ToolRunRequest::new(
            &task,
            tool_calls,
            &mut context,
            &mut messages,
            &mut cycle,
        ))
        .expect("tool run");

    assert_eq!(cycle.tool_results.len(), 2);
    assert_eq!(cycle.tool_results[0].directive, ToolDirective::Continue);
    assert_eq!(cycle.tool_results[1].directive, ToolDirective::Finish);
    assert_eq!(
        outcome.directive_result.expect("directive").content,
        "publish output"
    );
}

fn empty_cycle(tool_calls: Vec<ToolCall>) -> vv_agent::CycleRecord {
    vv_agent::CycleRecord {
        index: 1,
        assistant_message: String::new(),
        tool_calls,
        tool_results: Vec::new(),
        memory_compacted: false,
        token_usage: vv_agent::TokenUsage::default(),
    }
}

#[test]
fn tool_call_runner_short_circuit_result_keeps_original_tool_call_id_after_call_patch() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = ToolRegistry::new();
    let runner =
        ToolCallRunner::new(registry).with_hook_manager(RuntimeHookManager::new(vec![Arc::new(
            PatchedCallAndBlankResultHook,
        )]));
    let task = vv_agent::AgentTask::new("tool_runner_hook", "demo", "system", "prompt");
    let mut context = ToolContext::new(workspace.path());
    let tool_calls = vec![ToolCall::new(
        "original_call",
        "_patched_by_hook",
        BTreeMap::new(),
    )];
    let mut messages = Vec::new();
    let mut cycle = vv_agent::CycleRecord {
        index: 1,
        assistant_message: String::new(),
        tool_calls: tool_calls.clone(),
        tool_results: Vec::new(),
        memory_compacted: false,
        token_usage: vv_agent::TokenUsage::default(),
    };

    runner
        .run(ToolRunRequest::new(
            &task,
            tool_calls,
            &mut context,
            &mut messages,
            &mut cycle,
        ))
        .expect("tool run");

    assert_eq!(cycle.tool_results.len(), 1);
    assert_eq!(cycle.tool_results[0].tool_call_id, "original_call");
    assert_eq!(
        messages[0].tool_call_id.as_deref(),
        Some("original_call"),
        "tool messages must answer the model's original tool call id even when a hook patches the call"
    );
}

struct PatchedCallAndBlankResultHook;

impl RuntimeHook for PatchedCallAndBlankResultHook {
    fn before_tool_call(&self, event: BeforeToolCallEvent<'_>) -> Option<BeforeToolCallPatch> {
        let mut patched = event.call.clone();
        patched.id = "patched_call".to_string();
        let result = ToolExecutionResult::success("", json!({"ok": true}).to_string());
        Some(BeforeToolCallPatch {
            call: Some(patched),
            result: Some(result),
        })
    }
}
