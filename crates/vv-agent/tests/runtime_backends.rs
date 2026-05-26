use serde_json::{json, Value};
use vv_agent::runtime::backends::{CeleryBackend, InlineBackend, RuntimeRecipe, ThreadBackend};
use vv_agent::{
    AgentResult, AgentStatus, AgentTask, CancellationToken, CycleRecord, LLMResponse, Message,
    TaskTokenUsage,
};

#[test]
fn inline_backend_parallel_map_runs_serially_and_preserves_order() {
    let backend = InlineBackend;

    let results = backend.parallel_map(|value| value * 2, vec![1, 2, 3, 4]);

    assert_eq!(results, vec![2, 4, 6, 8]);
}

#[test]
fn thread_backend_submit_and_parallel_map_preserve_order() {
    let backend = ThreadBackend::new(2);

    let future = backend.submit(|| 42);
    let results = backend.parallel_map(|value| value * 2, vec![1, 2, 3, 4]);

    assert_eq!(future.join().expect("thread result"), 42);
    assert_eq!(results, vec![2, 4, 6, 8]);
}

#[test]
fn runtime_recipe_round_trips_through_json() {
    let recipe = RuntimeRecipe {
        settings_file: "/tmp/settings.py".to_string(),
        backend: "deepseek".to_string(),
        model: "deepseek-v4-pro".to_string(),
        workspace: "/tmp/workspace".to_string(),
        timeout_seconds: 120.0,
        hook_class_paths: vec!["my.hooks.LogHook".to_string()],
        log_preview_chars: Some(300),
    };

    let payload = serde_json::to_value(&recipe).expect("serialize");
    assert_eq!(payload["backend"], json!("deepseek"));

    let restored: RuntimeRecipe = serde_json::from_value(payload).expect("deserialize");
    assert_eq!(restored, recipe);
}

#[test]
fn celery_backend_without_dispatcher_keeps_inline_parallel_map_fallback() {
    let backend = CeleryBackend::inline_fallback();

    let results = backend.parallel_map(|value| value * 3, vec![1, 2, 3]);

    assert_eq!(results, vec![3, 6, 9]);
    assert!(backend.runtime_recipe().is_none());
}

#[test]
fn inline_backend_execute_runs_python_style_cycle_loop() {
    let backend = InlineBackend;
    let task = AgentTask::new("backend-loop", "model", "system", "prompt");
    let initial_messages = vec![Message::user("hello")];

    let result = backend.execute(
        &task,
        initial_messages,
        Default::default(),
        |cycle_index, messages, cycles, shared_state, _cancellation| {
            messages.push(Message::assistant(format!("cycle-{cycle_index}")));
            cycles.push(CycleRecord::from_response(
                cycle_index,
                &LLMResponse::new("assistant"),
                vec![],
            ));
            shared_state.insert("last_cycle".to_string(), Value::from(cycle_index));
            if cycle_index == 2 {
                Some(AgentResult::completed_with_shared_state(
                    messages.clone(),
                    cycles.clone(),
                    "done",
                    shared_state.clone(),
                ))
            } else {
                None
            }
        },
        None,
        4,
    );

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(result.final_answer.as_deref(), Some("done"));
    assert_eq!(result.cycles[0].index, 1);
    assert_eq!(result.cycles[1].index, 2);
    assert_eq!(result.shared_state["last_cycle"], Value::from(2));
}

#[test]
fn inline_backend_execute_returns_python_style_max_cycles_result() {
    let backend = InlineBackend;
    let task = AgentTask::new("backend-max", "model", "system", "prompt");

    let result = backend.execute(
        &task,
        vec![],
        Default::default(),
        |cycle_index, _messages, cycles, _shared_state, _cancellation| {
            cycles.push(CycleRecord::from_response(
                cycle_index,
                &LLMResponse::new("assistant"),
                vec![],
            ));
            None
        },
        None,
        2,
    );

    assert_eq!(result.status, AgentStatus::MaxCycles);
    assert_eq!(
        result.final_answer.as_deref(),
        Some("Reached max cycles without finish signal.")
    );
    assert_eq!(result.cycles.len(), 2);
    assert_eq!(result.token_usage, TaskTokenUsage::default());
}

#[test]
fn thread_backend_execute_honors_cancellation_before_cycle() {
    let backend = ThreadBackend::default();
    let task = AgentTask::new("backend-cancel", "model", "system", "prompt");
    let token = CancellationToken::default();
    token.cancel();

    let result = backend.execute(
        &task,
        vec![Message::user("hello")],
        Default::default(),
        |_cycle_index, _messages, _cycles, _shared_state, _cancellation| {
            panic!("cancelled backend should not run cycle executor");
        },
        Some(&token),
        2,
    );

    assert_eq!(result.status, AgentStatus::Failed);
    assert_eq!(result.error.as_deref(), Some("Operation was cancelled"));
    assert_eq!(result.messages.len(), 1);
}

#[test]
fn celery_backend_inline_execute_matches_inline_fallback() {
    let backend = CeleryBackend::inline_fallback();
    let task = AgentTask::new("backend-celery-inline", "model", "system", "prompt");

    let result = backend.execute(
        &task,
        vec![],
        Default::default(),
        |cycle_index, messages, cycles, shared_state, _cancellation| {
            messages.push(Message::assistant("finished"));
            cycles.push(CycleRecord::from_response(
                cycle_index,
                &LLMResponse::new("assistant"),
                vec![],
            ));
            Some(AgentResult::completed_with_shared_state(
                messages.clone(),
                cycles.clone(),
                "celery-inline",
                shared_state.clone(),
            ))
        },
        None,
        3,
    );

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(result.final_answer.as_deref(), Some("celery-inline"));
    assert_eq!(result.cycles[0].index, 1);
}
