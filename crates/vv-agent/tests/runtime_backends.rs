use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use vv_agent::runtime::backends::{
    run_checkpointed_cycle, CeleryBackend, CycleTaskDispatchResult, CycleTaskDispatcher,
    InlineBackend, RuntimeExecutionBackend, RuntimeRecipe, ThreadBackend,
};
use vv_agent::runtime::state::{Checkpoint, InMemoryStateStore, StateStore};
use vv_agent::{
    AgentResult, AgentRuntime, AgentStatus, AgentTask, CancellationToken, CycleRecord, LLMResponse,
    Message, ScriptedLlmClient, TaskTokenUsage,
};

#[test]
fn runtime_backends_exports_agent_base_execution_backend_paths() {
    let _direct = vv_agent::runtime::backends::ExecutionBackend::default();
    let _base = vv_agent::runtime::backends::base::ExecutionBackend::default();
}

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
fn runtime_recipe_matches_dict_and_default_checkpoint_path() {
    let recipe = RuntimeRecipe::new(
        "/tmp/settings.py",
        "deepseek",
        "deepseek-v4-pro",
        "/tmp/vv-agent-workspace",
    );

    let payload = recipe.to_dict();
    assert_eq!(payload["settings_file"], json!("/tmp/settings.py"));
    assert_eq!(payload["timeout_seconds"], json!(90.0));

    let restored = RuntimeRecipe::from_dict(&payload).expect("runtime recipe from dict");
    assert_eq!(restored, recipe);
    assert!(restored
        .default_sqlite_checkpoint_path()
        .ends_with(".vv-agent-state/checkpoints.db"));
}

#[test]
fn cycle_task_dispatch_result_matches_worker_payload_shape() {
    let result = AgentResult::completed(vec![Message::assistant("done")], Vec::new(), "ok");
    let terminal = CycleTaskDispatchResult::finished(result.clone());

    let payload = terminal.to_dict();
    assert_eq!(payload["finished"], json!(true));
    assert_eq!(payload["result"]["status"], json!("completed"));
    assert_eq!(payload["result"]["final_answer"], json!("ok"));

    let restored = CycleTaskDispatchResult::from_dict(&payload).expect("dispatch result");
    assert!(restored.finished);
    assert_eq!(restored.result, Some(result));

    let unfinished_payload = CycleTaskDispatchResult::unfinished().to_dict();
    assert_eq!(unfinished_payload, json!({"finished": false}));
    let unfinished = CycleTaskDispatchResult::from_dict(&unfinished_payload)
        .expect("unfinished dispatch result");
    assert!(!unfinished.finished);
    assert!(unfinished.result.is_none());
}

#[test]
fn celery_backend_without_dispatcher_keeps_inline_parallel_map_fallback() {
    let backend = CeleryBackend::inline_fallback();

    let results = backend.parallel_map(|value| value * 3, vec![1, 2, 3]);

    assert_eq!(results, vec![3, 6, 9]);
    assert!(backend.runtime_recipe().is_none());
}

#[test]
fn inline_backend_execute_runs_agent_cycle_loop() {
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
fn inline_backend_execute_returns_agent_max_cycles_result() {
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

#[test]
fn celery_backend_distributed_requires_store_and_dispatcher() {
    let backend = CeleryBackend::distributed(RuntimeRecipe::new(
        "settings.py",
        "deepseek",
        "deepseek-v4-pro",
        ".",
    ));
    let task = AgentTask::new(
        "distributed-misconfigured",
        "deepseek-v4-pro",
        "system",
        "prompt",
    );

    let result = backend.execute(
        &task,
        vec![Message::user("hello")],
        Default::default(),
        |_cycle_index, _messages, _cycles, _shared_state, _cancellation| {
            panic!("misconfigured distributed backend should not fall back to inline execution")
        },
        None,
        1,
    );

    assert_eq!(result.status, AgentStatus::Failed);
    assert_eq!(result.messages[0].content, "hello");
    assert!(result
        .error
        .as_deref()
        .is_some_and(|error| error.contains("requires a state_store and cycle_dispatcher")));
}

#[derive(Debug)]
struct RecordingDispatcher {
    store: Arc<InMemoryStateStore>,
    calls: Arc<Mutex<Vec<(String, u32)>>>,
}

impl CycleTaskDispatcher for RecordingDispatcher {
    fn dispatch_cycle(
        &self,
        task: &AgentTask,
        _recipe: &RuntimeRecipe,
        cycle_task_name: &str,
        cycle_index: u32,
    ) -> Result<CycleTaskDispatchResult, String> {
        self.calls
            .lock()
            .expect("calls")
            .push((cycle_task_name.to_string(), cycle_index));
        let mut checkpoint = self
            .store
            .load_checkpoint(&task.task_id)
            .expect("load checkpoint")
            .expect("checkpoint exists");
        if cycle_index == 1 {
            assert_eq!(checkpoint.cycle_index, 0);
            checkpoint.cycle_index = 1;
            checkpoint
                .messages
                .push(Message::assistant("worker cycle 1"));
            checkpoint.cycles.push(CycleRecord::from_response(
                cycle_index,
                &LLMResponse::new("worker cycle 1"),
                vec![],
            ));
            checkpoint
                .shared_state
                .insert("worker_cycle".to_string(), Value::from(cycle_index));
            self.store
                .save_checkpoint(checkpoint)
                .expect("save cycle 1");
            Ok(CycleTaskDispatchResult::unfinished())
        } else {
            assert_eq!(checkpoint.cycle_index, 1);
            Ok(CycleTaskDispatchResult::finished(
                AgentResult::completed_with_shared_state(
                    checkpoint.messages,
                    checkpoint.cycles,
                    "distributed done",
                    checkpoint.shared_state,
                ),
            ))
        }
    }
}

#[test]
fn celery_backend_distributed_dispatches_cycles_through_checkpoint_store() {
    let recipe = RuntimeRecipe::new("settings.py", "deepseek", "deepseek-v4-pro", ".");
    let store = Arc::new(InMemoryStateStore::new());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let dispatcher = Arc::new(RecordingDispatcher {
        store: store.clone(),
        calls: calls.clone(),
    });
    let backend = CeleryBackend::distributed_with_dispatcher(recipe, store.clone(), dispatcher)
        .with_cycle_task_name("custom.run_cycle");
    let task = AgentTask::new("distributed-task", "deepseek-v4-pro", "system", "prompt");

    let result = backend.execute(
        &task,
        vec![Message::user("hello")],
        [("seed".to_string(), json!("state"))].into_iter().collect(),
        |_cycle_index, _messages, _cycles, _shared_state, _cancellation| {
            panic!("distributed backend should dispatch worker cycles")
        },
        None,
        3,
    );

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(result.final_answer.as_deref(), Some("distributed done"));
    assert_eq!(result.cycles[0].index, 1);
    assert_eq!(result.shared_state["worker_cycle"], json!(1));
    assert_eq!(
        *calls.lock().expect("calls"),
        vec![
            ("custom.run_cycle".to_string(), 1),
            ("custom.run_cycle".to_string(), 2),
        ]
    );
    assert!(store
        .load_checkpoint(&task.task_id)
        .expect("load after cleanup")
        .is_none());
}

#[test]
fn runtime_delegates_cycle_execution_to_configured_backend() {
    let recipe = RuntimeRecipe::new("settings.py", "deepseek", "deepseek-v4-pro", ".");
    let store = Arc::new(InMemoryStateStore::new());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let dispatcher = Arc::new(RecordingDispatcher {
        store: store.clone(),
        calls: calls.clone(),
    });
    let backend = CeleryBackend::distributed_with_dispatcher(recipe, store.clone(), dispatcher)
        .with_cycle_task_name("custom.run_cycle");
    let runtime = AgentRuntime::new(ScriptedLlmClient::new(Vec::new()))
        .with_execution_backend(RuntimeExecutionBackend::Celery(backend));
    let task = AgentTask::new(
        "runtime-distributed-task",
        "deepseek-v4-pro",
        "system",
        "prompt",
    );

    let result = runtime.run(task).expect("distributed runtime result");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(result.final_answer.as_deref(), Some("distributed done"));
    assert_eq!(
        *calls.lock().expect("calls"),
        vec![
            ("custom.run_cycle".to_string(), 1),
            ("custom.run_cycle".to_string(), 2),
        ]
    );
    assert!(store
        .load_checkpoint("runtime-distributed-task")
        .expect("load after cleanup")
        .is_none());
}

#[derive(Debug)]
struct MetadataSnapshotDispatcher {
    seen_bash_hint: Arc<Mutex<Option<Value>>>,
}

impl CycleTaskDispatcher for MetadataSnapshotDispatcher {
    fn dispatch_cycle(
        &self,
        task: &AgentTask,
        _recipe: &RuntimeRecipe,
        _cycle_task_name: &str,
        _cycle_index: u32,
    ) -> Result<CycleTaskDispatchResult, String> {
        *self.seen_bash_hint.lock().expect("seen hint") =
            task.metadata.get("_vv_agent_bash_runtime_hint").cloned();
        Ok(CycleTaskDispatchResult::finished(
            AgentResult::completed_with_shared_state(
                Vec::new(),
                Vec::new(),
                "metadata captured",
                Default::default(),
            ),
        ))
    }
}

#[test]
fn runtime_freezes_dynamic_tool_schema_hints_before_distributed_dispatch() {
    let recipe = RuntimeRecipe::new("settings.py", "deepseek", "deepseek-v4-pro", ".");
    let store = Arc::new(InMemoryStateStore::new());
    let seen_bash_hint = Arc::new(Mutex::new(None));
    let dispatcher = Arc::new(MetadataSnapshotDispatcher {
        seen_bash_hint: seen_bash_hint.clone(),
    });
    let backend = CeleryBackend::distributed_with_dispatcher(recipe, store, dispatcher);
    let runtime = AgentRuntime::new(ScriptedLlmClient::new(Vec::new()))
        .with_execution_backend(RuntimeExecutionBackend::Celery(backend));
    let mut task = AgentTask::new("runtime-frozen-hint", "deepseek-v4-pro", "system", "prompt");
    task.agent_type = Some("computer".to_string());
    task.metadata
        .insert("bash_shell".to_string(), json!("bash"));

    let result = runtime.run(task).expect("distributed runtime result");

    assert_eq!(result.status, AgentStatus::Completed);
    let cached = seen_bash_hint
        .lock()
        .expect("seen hint")
        .clone()
        .and_then(|value| value.as_str().map(str::to_string))
        .expect("cached hint passed to backend");
    assert!(cached.contains("Runtime shell hint:"));
    assert!(cached.contains("bash"));
}

#[derive(Debug)]
struct FailingDispatcher;

impl CycleTaskDispatcher for FailingDispatcher {
    fn dispatch_cycle(
        &self,
        _task: &AgentTask,
        _recipe: &RuntimeRecipe,
        _cycle_task_name: &str,
        cycle_index: u32,
    ) -> Result<CycleTaskDispatchResult, String> {
        Err(format!("worker unavailable at {cycle_index}"))
    }
}

#[test]
fn celery_backend_distributed_returns_checkpointed_failure_and_cleans_up() {
    let recipe = RuntimeRecipe::new("settings.py", "deepseek", "deepseek-v4-pro", ".");
    let store = Arc::new(InMemoryStateStore::new());
    let backend = CeleryBackend::distributed_with_dispatcher(
        recipe,
        store.clone(),
        Arc::new(FailingDispatcher),
    );
    let task = AgentTask::new("distributed-fail", "deepseek-v4-pro", "system", "prompt");

    let result = backend.execute(
        &task,
        vec![Message::user("hello")],
        [("seed".to_string(), json!("state"))].into_iter().collect(),
        |_cycle_index, _messages, _cycles, _shared_state, _cancellation| {
            panic!("distributed backend should dispatch worker cycles")
        },
        None,
        2,
    );

    assert_eq!(result.status, AgentStatus::Failed);
    assert_eq!(
        result.error.as_deref(),
        Some("Celery cycle 1 failed: worker unavailable at 1")
    );
    assert_eq!(result.messages[0].content, "hello");
    assert_eq!(result.shared_state["seed"], json!("state"));
    assert!(store
        .load_checkpoint(&task.task_id)
        .expect("load after cleanup")
        .is_none());
}

#[derive(Debug)]
struct AdvancingDispatcher {
    store: Arc<InMemoryStateStore>,
}

impl CycleTaskDispatcher for AdvancingDispatcher {
    fn dispatch_cycle(
        &self,
        task: &AgentTask,
        _recipe: &RuntimeRecipe,
        _cycle_task_name: &str,
        cycle_index: u32,
    ) -> Result<CycleTaskDispatchResult, String> {
        let mut checkpoint = self
            .store
            .load_checkpoint(&task.task_id)
            .expect("load checkpoint")
            .expect("checkpoint exists");
        checkpoint.cycle_index = cycle_index;
        checkpoint.cycles.push(CycleRecord::from_response(
            cycle_index,
            &LLMResponse::new("worker"),
            vec![],
        ));
        self.store.save_checkpoint(checkpoint).expect("save cycle");
        Ok(CycleTaskDispatchResult::unfinished())
    }
}

#[test]
fn celery_backend_distributed_returns_checkpointed_max_cycles_and_cleans_up() {
    let recipe = RuntimeRecipe::new("settings.py", "deepseek", "deepseek-v4-pro", ".");
    let store = Arc::new(InMemoryStateStore::new());
    let dispatcher = Arc::new(AdvancingDispatcher {
        store: store.clone(),
    });
    let backend = CeleryBackend::distributed_with_dispatcher(recipe, store.clone(), dispatcher);
    let task = AgentTask::new("distributed-max", "deepseek-v4-pro", "system", "prompt");

    let result = backend.execute(
        &task,
        vec![Message::user("hello")],
        Default::default(),
        |_cycle_index, _messages, _cycles, _shared_state, _cancellation| {
            panic!("distributed backend should dispatch worker cycles")
        },
        None,
        2,
    );

    assert_eq!(result.status, AgentStatus::MaxCycles);
    assert_eq!(result.cycles.len(), 2);
    assert_eq!(
        result.final_answer.as_deref(),
        Some("Reached max cycles without finish signal.")
    );
    assert!(store
        .load_checkpoint(&task.task_id)
        .expect("load after cleanup")
        .is_none());
}

#[test]
fn checkpointed_cycle_worker_returns_failed_result_when_checkpoint_is_missing() {
    let store = InMemoryStateStore::new();
    let task = AgentTask::new("missing-worker-task", "model", "system", "prompt");

    let dispatch_result = run_checkpointed_cycle(
        &store,
        &task,
        1,
        |_cycle_index, _messages, _cycles, _shared_state, _cancellation| {
            panic!("worker should not execute without checkpoint")
        },
    )
    .expect("worker result");

    assert!(dispatch_result.finished);
    let result = dispatch_result.result.expect("failed result");
    assert_eq!(result.status, AgentStatus::Failed);
    assert_eq!(
        result.error.as_deref(),
        Some("No checkpoint found for task missing-worker-task")
    );
}

#[test]
fn checkpointed_cycle_worker_saves_checkpoint_after_nonterminal_cycle() {
    let store = InMemoryStateStore::new();
    let task = AgentTask::new("worker-task", "model", "system", "prompt");
    store
        .save_checkpoint(Checkpoint {
            task_id: task.task_id.clone(),
            cycle_index: 0,
            status: AgentStatus::Running,
            messages: vec![Message::user("hello")],
            cycles: Vec::new(),
            shared_state: Default::default(),
        })
        .expect("save checkpoint");

    let dispatch_result = run_checkpointed_cycle(
        &store,
        &task,
        1,
        |cycle_index, messages, cycles, shared_state, _cancellation| {
            messages.push(Message::assistant("worker response"));
            cycles.push(CycleRecord::from_response(
                cycle_index,
                &LLMResponse::new("worker response"),
                vec![],
            ));
            shared_state.insert("worker".to_string(), json!("updated"));
            None
        },
    )
    .expect("worker result");

    assert!(!dispatch_result.finished);
    assert!(dispatch_result.result.is_none());
    let checkpoint = store
        .load_checkpoint(&task.task_id)
        .expect("load checkpoint")
        .expect("checkpoint exists");
    assert_eq!(checkpoint.cycle_index, 1);
    assert_eq!(
        checkpoint.messages.last().unwrap().content,
        "worker response"
    );
    assert_eq!(checkpoint.cycles[0].index, 1);
    assert_eq!(checkpoint.shared_state["worker"], json!("updated"));
}

#[test]
fn checkpointed_cycle_worker_deletes_checkpoint_after_terminal_result() {
    let store = InMemoryStateStore::new();
    let task = AgentTask::new("worker-terminal", "model", "system", "prompt");
    store
        .save_checkpoint(Checkpoint {
            task_id: task.task_id.clone(),
            cycle_index: 0,
            status: AgentStatus::Running,
            messages: vec![Message::user("hello")],
            cycles: Vec::new(),
            shared_state: Default::default(),
        })
        .expect("save checkpoint");

    let dispatch_result = run_checkpointed_cycle(
        &store,
        &task,
        1,
        |cycle_index, messages, cycles, shared_state, _cancellation| {
            cycles.push(CycleRecord::from_response(
                cycle_index,
                &LLMResponse::new("done"),
                vec![],
            ));
            Some(AgentResult::completed_with_shared_state(
                messages.clone(),
                cycles.clone(),
                "worker done",
                shared_state.clone(),
            ))
        },
    )
    .expect("worker result");

    assert!(dispatch_result.finished);
    assert_eq!(
        dispatch_result
            .result
            .as_ref()
            .and_then(|result| result.final_answer.as_deref()),
        Some("worker done")
    );
    assert!(store
        .load_checkpoint(&task.task_id)
        .expect("load after delete")
        .is_none());
}
