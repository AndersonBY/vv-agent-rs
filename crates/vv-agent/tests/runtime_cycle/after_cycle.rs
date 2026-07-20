use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

use super::*;
use vv_agent::{
    runtime::lifecycle::{AFTER_CYCLE_CONTROL_SCHEMA, AFTER_CYCLE_CONTROL_STATE_KEY},
    AfterCycleAction, AfterCycleDecision, AfterCycleHook, AfterCycleSnapshot, CompletionReason,
    NativeCycleOutcomeKind, NoToolPolicy, ScriptStep,
};

#[derive(Default)]
struct SteeringHook {
    calls: AtomicUsize,
    snapshots: Mutex<Vec<AfterCycleSnapshot>>,
}

impl AfterCycleHook for SteeringHook {
    fn after_cycle(
        &self,
        snapshot: &AfterCycleSnapshot,
    ) -> Result<Option<AfterCycleDecision>, String> {
        self.snapshots
            .lock()
            .expect("snapshot lock")
            .push(snapshot.clone());
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return AfterCycleDecision::steer(["Check the output once more."])
                .map(Some)
                .map_err(|error| error.to_string());
        }
        Ok(Some(AfterCycleDecision::continue_run()))
    }
}

#[test]
fn after_cycle_steer_defers_native_no_tool_completion() {
    let hook = Arc::new(SteeringHook::default());
    let llm = ScriptedLlmClient::new(vec![
        LLMResponse::new("first answer"),
        LLMResponse::new("checked answer"),
    ]);
    let runtime = AgentRuntime::new(llm).with_after_cycle_hooks(vec![hook.clone()]);
    let mut task = AgentTask::new("steer_task", "demo", "system", "answer");
    task.no_tool_policy = NoToolPolicy::Finish;
    task.max_cycles = 3;

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.completion_reason,
        Some(CompletionReason::NoToolFinish)
    );
    assert_eq!(result.final_answer.as_deref(), Some("checked answer"));
    assert_eq!(result.cycles.len(), 2);
    assert_eq!(hook.calls.load(Ordering::SeqCst), 2);
    let snapshots = hook.snapshots.lock().expect("snapshot lock");
    assert_eq!(
        snapshots[0].native_outcome.kind,
        NativeCycleOutcomeKind::Completed
    );
    assert!(snapshots[0].native_outcome.steer_allowed);
    assert_eq!(snapshots[0].remaining_cycles, 2);
    assert!(result.messages.iter().any(|message| {
        message.role == vv_agent::MessageRole::User
            && message.content == "Check the output once more."
    }));
    assert!(!result
        .messages
        .iter()
        .any(|message| message.content == "Continue. If the task is complete, call task_finish."));
}

#[test]
fn after_cycle_stop_cannot_project_tool_completion_as_success() {
    let hook = Arc::new(|_snapshot: &AfterCycleSnapshot| {
        AfterCycleDecision::stop_non_success("host.policy_stop", "Host policy stopped this run.")
            .map(Some)
            .map_err(|error| error.to_string())
    });
    let response = LLMResponse::with_tool_calls(
        "done",
        vec![ToolCall::new(
            "finish",
            "task_finish",
            BTreeMap::from([("message".to_string(), json!("native success"))]),
        )],
    );
    let runtime = AgentRuntime::new(ScriptedLlmClient::new(vec![response]))
        .with_after_cycle_hooks(vec![hook]);

    let result = runtime
        .run(AgentTask::new("stop_task", "demo", "system", "finish"))
        .expect("run");

    assert_eq!(result.status, AgentStatus::Failed);
    assert_eq!(result.completion_reason, Some(CompletionReason::Failed));
    assert!(result.final_answer.is_none());
    assert_eq!(
        result.error.as_deref(),
        Some("host.policy_stop: Host policy stopped this run.")
    );
}

#[test]
fn after_cycle_steer_at_max_cycles_fails_closed() {
    let hook = Arc::new(|_snapshot: &AfterCycleSnapshot| {
        AfterCycleDecision::steer(["Try again."])
            .map(Some)
            .map_err(|error| error.to_string())
    });
    let runtime = AgentRuntime::new(ScriptedLlmClient::new(vec![LLMResponse::new("partial")]))
        .with_after_cycle_hooks(vec![hook]);
    let mut task = AgentTask::new("max_task", "demo", "system", "work");
    task.no_tool_policy = NoToolPolicy::Continue;
    task.max_cycles = 1;

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Failed);
    assert!(result
        .error
        .as_deref()
        .is_some_and(|error| error.starts_with("after_cycle_steer_unavailable:")));
}

#[test]
fn after_cycle_permission_narrowing_hides_schema_and_blocks_dispatch() {
    let calls = Arc::new(AtomicUsize::new(0));
    let hook_calls = calls.clone();
    let hook = Arc::new(move |_snapshot: &AfterCycleSnapshot| {
        let call = hook_calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return AfterCycleDecision::continue_with_disallowed_tools(["bash"])
                .map(Some)
                .map_err(|error| error.to_string());
        }
        Ok(Some(AfterCycleDecision::continue_run()))
    });
    let inspect = ScriptStep::callback(|request| {
        let names = request
            .tools
            .iter()
            .filter_map(|schema| schema.pointer("/function/name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert!(!names.contains(&"bash"));
        Ok(LLMResponse::with_tool_calls(
            "try hidden tool",
            vec![ToolCall::new(
                "bash_call",
                "bash",
                BTreeMap::from([("command".to_string(), json!("echo forbidden"))]),
            )],
        ))
    });
    let finish = LLMResponse::with_tool_calls(
        "done",
        vec![ToolCall::new(
            "finish",
            "task_finish",
            BTreeMap::from([("message".to_string(), json!("done"))]),
        )],
    );
    let llm = ScriptedLlmClient::from_steps(vec![
        ScriptStep::response(LLMResponse::new("observe")),
        inspect,
        ScriptStep::response(finish),
    ]);
    let runtime = AgentRuntime::new(llm).with_after_cycle_hooks(vec![hook]);
    let mut task = AgentTask::new("deny_task", "demo", "system", "work");
    task.no_tool_policy = NoToolPolicy::Continue;
    task.max_cycles = 4;
    task.use_workspace = true;

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.cycles[1].tool_results[0].error_code.as_deref(),
        Some("tool_not_allowed")
    );
    assert_eq!(
        result.shared_state[AFTER_CYCLE_CONTROL_STATE_KEY],
        json!({
            "schema_version": AFTER_CYCLE_CONTROL_SCHEMA,
            "disallowed_tools": ["bash"],
        })
    );
}

#[test]
fn invalid_after_cycle_control_state_fails_before_model_call() {
    let runtime = AgentRuntime::new(ScriptedLlmClient::new(vec![LLMResponse::new("unused")]));
    let mut task = AgentTask::new("invalid_state", "demo", "system", "work");
    task.initial_shared_state.insert(
        AFTER_CYCLE_CONTROL_STATE_KEY.to_string(),
        json!({
            "schema_version": AFTER_CYCLE_CONTROL_SCHEMA,
            "disallowed_tools": ["z", "a"],
        }),
    );

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Failed);
    assert!(result.cycles.is_empty());
    assert!(result
        .error
        .as_deref()
        .is_some_and(|error| error.starts_with("after_cycle_control_state_invalid:")));
}

#[test]
fn after_cycle_manager_composes_same_snapshot_in_registration_order() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let first_observed = observed.clone();
    let first = Arc::new(move |snapshot: &AfterCycleSnapshot| {
        first_observed
            .lock()
            .expect("observed lock")
            .push(("first", snapshot.messages.len()));
        AfterCycleDecision::steer_with_disallowed_tools(["first"], ["bash"])
            .map(Some)
            .map_err(|error| error.to_string())
    });
    let second_observed = observed.clone();
    let second = Arc::new(move |snapshot: &AfterCycleSnapshot| {
        second_observed
            .lock()
            .expect("observed lock")
            .push(("second", snapshot.messages.len()));
        AfterCycleDecision::steer_with_disallowed_tools(["second"], ["read_file"])
            .map(Some)
            .map_err(|error| error.to_string())
    });
    let manager = vv_agent::runtime::lifecycle::AfterCycleHookManager::new(vec![first, second]);
    let snapshot = AfterCycleSnapshot::capture(
        "compose",
        1,
        3,
        &vv_agent::CycleRecord {
            index: 1,
            assistant_message: "answer".to_string(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            memory_compacted: false,
            token_usage: TokenUsage::default(),
        },
        &[Message::assistant("answer")],
        &BTreeMap::new(),
        vv_agent::TaskTokenUsage::default(),
        Vec::new(),
        Vec::new(),
        vv_agent::NativeCycleOutcome::continuing(),
    );

    let decision = manager.apply(&snapshot).expect("composed decision");

    assert_eq!(decision.action, AfterCycleAction::Steer);
    assert_eq!(decision.steering_messages, ["first", "second"]);
    assert_eq!(decision.disallow_tools, ["bash", "read_file"]);
    assert_eq!(
        *observed.lock().expect("observed lock"),
        [("first", 1), ("second", 1)]
    );
}
