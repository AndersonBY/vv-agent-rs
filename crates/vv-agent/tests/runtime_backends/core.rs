#[test]
fn runtime_backends_exports_agent_base_execution_backend_paths() {
    let _direct = vv_agent::runtime::backends::RuntimeExecutionBackend::default();
    let _base = vv_agent::runtime::backends::base::RuntimeExecutionBackend::default();
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
        settings_file: "/tmp/settings.json".to_string(),
        backend: "deepseek".to_string(),
        model: "deepseek-v4-pro".to_string(),
        workspace: "/tmp/workspace".to_string(),
        timeout_seconds: 120.0,
        log_preview_chars: Some(300),
        state_store: None,
        capabilities: Default::default(),
    };

    let payload = serde_json::to_value(&recipe).expect("serialize");
    assert_eq!(payload["backend"], json!("deepseek"));
    let mut keys = payload
        .as_object()
        .expect("recipe payload object")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "backend",
            "capabilities",
            "log_preview_chars",
            "model",
            "settings_file",
            "state_store",
            "timeout_seconds",
            "workspace"
        ]
    );

    let restored: RuntimeRecipe = serde_json::from_value(payload).expect("deserialize");
    assert_eq!(restored, recipe);
}

#[test]
fn runtime_recipe_matches_dict_and_default_checkpoint_path() {
    let recipe = RuntimeRecipe::new(
        "/tmp/settings.json",
        "deepseek",
        "deepseek-v4-pro",
        "/tmp/vv-agent-workspace",
    );

    let payload = recipe.to_dict();
    assert_eq!(payload["settings_file"], json!("/tmp/settings.json"));
    assert_eq!(payload["timeout_seconds"], json!(90.0));

    let restored = RuntimeRecipe::from_dict(&payload).expect("runtime recipe from dict");
    assert_eq!(restored, recipe);
    assert!(restored
        .default_sqlite_checkpoint_path()
        .ends_with(".vv-agent-state/checkpoints.db"));
}

#[test]
fn cycle_dispatch_result_matches_worker_payload_shape() {
    let result = AgentResult::completed(vec![Message::assistant("done")], Vec::new(), "ok");
    let terminal = CycleDispatchResult::finished(result.clone());

    let payload = terminal.to_dict();
    assert_eq!(payload["finished"], json!(true));
    assert_eq!(payload["result"]["status"], json!("completed"));
    assert_eq!(payload["result"]["final_answer"], json!("ok"));

    let restored = CycleDispatchResult::from_dict(&payload).expect("dispatch result");
    assert!(restored.finished);
    assert_eq!(restored.result, Some(result));

    let unfinished_payload = CycleDispatchResult::unfinished().to_dict();
    assert_eq!(unfinished_payload, json!({"finished": false}));
    let unfinished =
        CycleDispatchResult::from_dict(&unfinished_payload).expect("unfinished dispatch result");
    assert!(!unfinished.finished);
    assert!(unfinished.result.is_none());
}

#[test]
fn distributed_backend_without_dispatcher_keeps_inline_parallel_map_fallback() {
    let backend = DistributedBackend::inline_fallback();

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
fn distributed_backend_inline_execute_matches_inline_fallback() {
    let backend = DistributedBackend::inline_fallback();
    let task = AgentTask::new("backend-distributed-inline", "model", "system", "prompt");

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
                "distributed-inline",
                shared_state.clone(),
            ))
        },
        None,
        3,
    );

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(result.final_answer.as_deref(), Some("distributed-inline"));
    assert_eq!(result.cycles[0].index, 1);
}

#[test]
fn distributed_backend_requires_store_and_dispatcher() {
    let backend = DistributedBackend::distributed(RuntimeRecipe::new(
        "settings.json",
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

impl CycleDispatcher for RecordingDispatcher {
    fn dispatch_cycle(
        &self,
        task: &AgentTask,
        _recipe: &RuntimeRecipe,
        cycle_name: &str,
        cycle_index: u32,
    ) -> Result<CycleDispatchResult, String> {
        self.calls
            .lock()
            .expect("calls")
            .push((cycle_name.to_string(), cycle_index));
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
            Ok(CycleDispatchResult::unfinished())
        } else {
            assert_eq!(checkpoint.cycle_index, 1);
            Ok(CycleDispatchResult::finished(
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
