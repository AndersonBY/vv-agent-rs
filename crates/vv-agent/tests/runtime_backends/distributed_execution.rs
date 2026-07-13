#[test]
fn distributed_backend_dispatches_cycles_through_checkpoint_store() {
    let recipe = RuntimeRecipe::new("settings.json", "deepseek", "deepseek-v4-pro", ".");
    let store = Arc::new(InMemoryStateStore::new());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let dispatcher = Arc::new(RecordingDispatcher {
        store: store.clone(),
        calls: calls.clone(),
    });
    let backend =
        DistributedBackend::distributed_with_dispatcher(recipe, store.clone(), dispatcher)
            .with_cycle_name("custom.run_cycle");
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
    let recipe = RuntimeRecipe::new("settings.json", "deepseek", "deepseek-v4-pro", ".");
    let store = Arc::new(InMemoryStateStore::new());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let dispatcher = Arc::new(RecordingDispatcher {
        store: store.clone(),
        calls: calls.clone(),
    });
    let backend =
        DistributedBackend::distributed_with_dispatcher(recipe, store.clone(), dispatcher)
            .with_cycle_name("custom.run_cycle");
    let runtime = AgentRuntime::new(ScriptedLlmClient::new(Vec::new()))
        .with_execution_backend(RuntimeExecutionBackend::Distributed(backend));
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

impl CycleDispatcher for MetadataSnapshotDispatcher {
    fn dispatch_cycle(
        &self,
        task: &AgentTask,
        _recipe: &RuntimeRecipe,
        _cycle_name: &str,
        _cycle_index: u32,
    ) -> Result<CycleDispatchResult, String> {
        *self.seen_bash_hint.lock().expect("seen hint") =
            task.metadata.get("_vv_agent_bash_runtime_hint").cloned();
        Ok(CycleDispatchResult::finished(
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
    let recipe = RuntimeRecipe::new("settings.json", "deepseek", "deepseek-v4-pro", ".");
    let store = Arc::new(InMemoryStateStore::new());
    let seen_bash_hint = Arc::new(Mutex::new(None));
    let dispatcher = Arc::new(MetadataSnapshotDispatcher {
        seen_bash_hint: seen_bash_hint.clone(),
    });
    let backend = DistributedBackend::distributed_with_dispatcher(recipe, store, dispatcher);
    let runtime = AgentRuntime::new(ScriptedLlmClient::new(Vec::new()))
        .with_execution_backend(RuntimeExecutionBackend::Distributed(backend));
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

impl CycleDispatcher for FailingDispatcher {
    fn dispatch_cycle(
        &self,
        _task: &AgentTask,
        _recipe: &RuntimeRecipe,
        _cycle_name: &str,
        cycle_index: u32,
    ) -> Result<CycleDispatchResult, String> {
        Err(format!("worker unavailable at {cycle_index}"))
    }
}

#[test]
fn distributed_backend_returns_checkpointed_failure_and_cleans_up() {
    let recipe = RuntimeRecipe::new("settings.json", "deepseek", "deepseek-v4-pro", ".");
    let store = Arc::new(InMemoryStateStore::new());
    let backend = DistributedBackend::distributed_with_dispatcher(
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
        Some("Distributed cycle 1 failed: worker unavailable at 1")
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

impl CycleDispatcher for AdvancingDispatcher {
    fn dispatch_cycle(
        &self,
        task: &AgentTask,
        _recipe: &RuntimeRecipe,
        _cycle_name: &str,
        cycle_index: u32,
    ) -> Result<CycleDispatchResult, String> {
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
        Ok(CycleDispatchResult::unfinished())
    }
}

#[test]
fn distributed_backend_returns_checkpointed_max_cycles_and_cleans_up() {
    let recipe = RuntimeRecipe::new("settings.json", "deepseek", "deepseek-v4-pro", ".");
    let store = Arc::new(InMemoryStateStore::new());
    let dispatcher = Arc::new(AdvancingDispatcher {
        store: store.clone(),
    });
    let backend =
        DistributedBackend::distributed_with_dispatcher(recipe, store.clone(), dispatcher);
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
fn distributed_backend_accepts_custom_cycle_name() {
    fn assert_dispatcher<T: CycleDispatcher>() {}

    #[derive(Debug)]
    struct CustomDispatcher;

    impl CycleDispatcher for CustomDispatcher {
        fn dispatch_cycle(
            &self,
            _task: &AgentTask,
            _recipe: &RuntimeRecipe,
            _cycle_name: &str,
            _cycle_index: u32,
        ) -> Result<CycleDispatchResult, String> {
            Ok(CycleDispatchResult::unfinished())
        }
    }

    assert_dispatcher::<CustomDispatcher>();
    let recipe = RuntimeRecipe::new("settings.json", "deepseek", "deepseek-v4-pro", ".");
    let store = Arc::new(InMemoryStateStore::new());
    let backend =
        DistributedBackend::distributed_with_dispatcher(recipe, store, Arc::new(CustomDispatcher))
            .with_cycle_name("custom.run_cycle");
    assert_eq!(backend.cycle_name(), "custom.run_cycle");
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
            revision: 0,
            claim_token: None,
            claimed_cycle: None,
            lease_expires_at_ms: None,
            terminal_result: None,
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
fn checkpointed_cycle_worker_persists_terminal_result_until_acknowledged() {
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
            revision: 0,
            claim_token: None,
            claimed_cycle: None,
            lease_expires_at_ms: None,
            terminal_result: None,
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
    let terminal = store
        .load_checkpoint(&task.task_id)
        .expect("load terminal checkpoint")
        .expect("terminal checkpoint");
    assert_eq!(terminal.terminal_result, dispatch_result.result);
    let revision = dispatch_result
        .checkpoint_revision
        .expect("terminal revision");
    assert!(store
        .acknowledge_terminal(&task.task_id, revision)
        .expect("ack terminal"));
    assert!(store
        .load_checkpoint(&task.task_id)
        .expect("load after ack")
        .is_none());
}

#[test]
fn checkpointed_cycle_worker_replays_persisted_terminal_without_executing_again() {
    let store = InMemoryStateStore::new();
    let task = AgentTask::new("worker-redelivery", "model", "system", "prompt");
    store
        .save_checkpoint(Checkpoint {
            task_id: task.task_id.clone(),
            cycle_index: 1,
            status: AgentStatus::Completed,
            messages: vec![Message::user("hello")],
            cycles: Vec::new(),
            shared_state: Default::default(),
            revision: 2,
            claim_token: None,
            claimed_cycle: None,
            lease_expires_at_ms: None,
            terminal_result: Some(AgentResult::completed(
                vec![Message::user("hello")],
                Vec::new(),
                "persisted done",
            )),
        })
        .expect("save terminal checkpoint");

    let dispatch_result = run_checkpointed_cycle(
        &store,
        &task,
        1,
        |_cycle_index, _messages, _cycles, _shared_state, _cancellation| {
            panic!("redelivery must not execute the cycle again")
        },
    )
    .expect("redelivered result");

    assert!(dispatch_result.finished);
    assert_eq!(dispatch_result.checkpoint_revision, Some(2));
    assert_eq!(
        dispatch_result
            .result
            .as_ref()
            .and_then(|result| result.final_answer.as_deref()),
        Some("persisted done")
    );
}
