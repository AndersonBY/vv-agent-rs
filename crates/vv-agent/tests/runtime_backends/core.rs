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
            "timeout_seconds",
            "workspace"
        ]
    );

    let restored: RuntimeRecipe = serde_json::from_value(payload).expect("deserialize");
    assert_eq!(restored, recipe);
}

#[test]
fn runtime_recipe_matches_current_wire_shape() {
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
}

const DISTRIBUTED_WORKER_RESPONSE_FIXTURE: &str =
    include_str!("../fixtures/parity/distributed_worker_response.json");

fn distributed_worker_response_fixture() -> Value {
    serde_json::from_str(DISTRIBUTED_WORKER_RESPONSE_FIXTURE)
        .expect("distributed worker response fixture")
}

fn response_fixture_case(fixture: &Value, case: &Value) -> Value {
    if let Some(response) = case.get("response") {
        return response.clone();
    }

    let base_name = case["base_valid_case"]
        .as_str()
        .expect("mutation base valid case");
    let mut response = fixture["valid_cases"]
        .as_array()
        .expect("valid cases")
        .iter()
        .find(|candidate| candidate["name"] == base_name)
        .expect("mutation base response")["response"]
        .clone();
    let mutation = &case["mutation"];
    let path = mutation["path"]
        .as_array()
        .expect("mutation path")
        .iter()
        .map(|field| field.as_str().expect("mutation path field"))
        .collect::<Vec<_>>();
    let (field, parents) = path.split_last().expect("non-empty mutation path");
    let mut object = response.as_object_mut().expect("response object");
    for parent in parents {
        object = object
            .get_mut(*parent)
            .and_then(Value::as_object_mut)
            .expect("mutation parent object");
    }

    match mutation["operation"].as_str().expect("mutation operation") {
        "add" => {
            assert!(
                object
                    .insert(
                        (*field).to_string(),
                        mutation.get("value").expect("mutation value").clone(),
                    )
                    .is_none(),
                "add mutation must target a missing field"
            );
        }
        "replace" => {
            assert!(
                object
                    .insert(
                        (*field).to_string(),
                        mutation.get("value").expect("mutation value").clone(),
                    )
                    .is_some(),
                "replace mutation must target an existing field"
            );
        }
        "remove" => {
            assert!(
                object.remove(*field).is_some(),
                "remove mutation must target an existing field"
            );
        }
        operation => panic!("unsupported response mutation {operation}"),
    }
    response
}

fn fixture_completed_result(response: &Value) -> AgentResult {
    let result_wire = &response["result"];
    let result = AgentResult::from_dict(result_wire).expect("fixture AgentResult");
    assert_eq!(result.to_dict(), *result_wire);
    result
}

#[test]
fn cycle_dispatch_result_consumes_every_valid_fixture_variant() {
    let fixture = distributed_worker_response_fixture();
    assert_eq!(
        vv_agent::runtime::backends::distributed::DISTRIBUTED_WORKER_RESPONSE_SCHEMA_VERSION,
        fixture["schema_version"]
    );

    for case in fixture["valid_cases"].as_array().expect("valid cases") {
        let name = case["name"].as_str().expect("case name");
        let response = &case["response"];
        let parsed = CycleDispatchResult::from_dict(response).expect(name);

        assert_eq!(parsed.kind(), name, "{name}");
        assert_eq!(parsed.to_dict(), *response, "{name}");
        assert_eq!(
            serde_json::from_value::<CycleDispatchResult>(response.clone()).expect(name),
            parsed,
            "{name}"
        );
    }
}

#[test]
fn cycle_dispatch_result_produces_every_valid_fixture_variant() {
    let fixture = distributed_worker_response_fixture();

    for case in fixture["valid_cases"].as_array().expect("valid cases") {
        let name = case["name"].as_str().expect("case name");
        let response = &case["response"];
        let produced = match name {
            "pending" => CycleDispatchResult::pending(),
            "committed" => CycleDispatchResult::committed(
                response["committed_cycle"]
                    .as_u64()
                    .expect("committed cycle"),
                response["checkpoint_revision"]
                    .as_u64()
                    .expect("checkpoint revision"),
            )
            .expect(name),
            "terminal_candidate" => CycleDispatchResult::terminal_candidate(
                fixture_completed_result(response),
                response["checkpoint_revision"]
                    .as_u64()
                    .expect("checkpoint revision"),
            )
            .expect(name),
            "terminal_replay" => CycleDispatchResult::terminal_replay(
                fixture_completed_result(response),
                response["checkpoint_revision"]
                    .as_u64()
                    .expect("checkpoint revision"),
            )
            .expect(name),
            other => panic!("unsupported valid fixture case {other}"),
        };

        assert_eq!(produced.to_dict(), *response, "{name}");
        assert_eq!(
            serde_json::to_value(&produced).expect(name),
            *response,
            "{name}"
        );
    }

    assert_eq!(
        CycleDispatchResult::committed(1, 0)
            .expect("zero checkpoint revision")
            .to_dict()["checkpoint_revision"],
        json!(0)
    );
}

#[test]
fn cycle_dispatch_result_rejects_every_invalid_fixture_case_and_invalid_producer() {
    let fixture = distributed_worker_response_fixture();
    let invalid_cases = fixture["invalid_cases"].as_array().expect("invalid cases");

    for case in invalid_cases {
        let name = case["name"].as_str().expect("case name");
        let expected = case["error"].as_str().expect("expected error");
        let response = response_fixture_case(&fixture, case);
        assert_eq!(
            CycleDispatchResult::from_dict(&response).unwrap_err(),
            expected,
            "{name}"
        );
        assert!(
            serde_json::from_value::<CycleDispatchResult>(response)
                .unwrap_err()
                .to_string()
                .contains(expected),
            "{name}"
        );
    }

    let expected = |name: &str| {
        invalid_cases
            .iter()
            .find(|case| case["name"] == name)
            .expect("invalid fixture case")["error"]
            .as_str()
            .expect("expected error")
    };
    assert_eq!(
        CycleDispatchResult::committed(0, 2).unwrap_err(),
        expected("committed_zero_cycle")
    );
    assert_eq!(
        CycleDispatchResult::committed(1, 1_u64 << 53).unwrap_err(),
        expected("committed_revision_above_wire_maximum")
    );
    assert_eq!(
        CycleDispatchResult::terminal_candidate(AgentResult::default(), 2).unwrap_err(),
        expected("terminal_candidate_invalid_result")
    );
    let reconciliation = AgentResult {
        status: AgentStatus::ReconciliationRequired,
        ..AgentResult::default()
    };
    let candidate = CycleDispatchResult::terminal_candidate(reconciliation.clone(), 2)
        .expect("reconciliation-required terminal candidate");
    let candidate_wire = candidate.to_dict();
    assert_eq!(
        CycleDispatchResult::from_dict(&candidate_wire).expect("parsed terminal candidate"),
        candidate
    );
    assert_eq!(
        CycleDispatchResult::terminal_replay(reconciliation.clone(), 2).unwrap_err(),
        expected("terminal_candidate_invalid_result")
    );

    let mut replay_wire = candidate_wire.clone();
    replay_wire["type"] = json!("terminal_replay");
    assert_eq!(
        CycleDispatchResult::from_dict(&replay_wire).unwrap_err(),
        expected("terminal_candidate_invalid_result")
    );

    let mut unknown_result_field = candidate_wire.clone();
    unknown_result_field["result"]["unexpected"] = json!(true);
    assert_eq!(
        CycleDispatchResult::from_dict(&unknown_result_field).unwrap_err(),
        expected("terminal_candidate_invalid_result")
    );

    let mut legacy_terminal_flags = candidate_wire.clone();
    legacy_terminal_flags["finished"] = json!(true);
    legacy_terminal_flags["terminal_candidate"] = json!(true);
    legacy_terminal_flags["terminal_replay"] = json!(false);
    assert_eq!(
        CycleDispatchResult::from_dict(&legacy_terminal_flags).unwrap_err(),
        "distributed worker response fields do not match type terminal_candidate"
    );

    let mut unsafe_revision = candidate_wire;
    unsafe_revision["checkpoint_revision"] = json!(1_u64 << 53);
    assert_eq!(
        CycleDispatchResult::from_dict(&unsafe_revision).unwrap_err(),
        "checkpoint_revision must be a JSON-safe unsigned integer"
    );
}

#[test]
fn cycle_dispatch_result_accepts_exactly_the_contract_status_matrix() {
    let fixture = distributed_worker_response_fixture();
    for matrix in fixture["status_matrix_cases"]
        .as_array()
        .expect("status matrix cases")
    {
        let response_type = matrix["type"].as_str().expect("response type");
        let base = fixture["valid_cases"]
            .as_array()
            .expect("valid cases")
            .iter()
            .find(|case| case["name"] == response_type)
            .expect("matching valid terminal case")["response"]
            .clone();
        for status in matrix["accepted_statuses"]
            .as_array()
            .expect("accepted statuses")
        {
            let mut response = base.clone();
            response["result"]["status"] = status.clone();
            let parsed = CycleDispatchResult::from_dict(&response)
                .unwrap_or_else(|error| panic!("{response_type}/{status}: {error}"));
            assert_eq!(parsed.kind(), response_type);
            assert_eq!(parsed.to_dict(), response);
        }
    }
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
    assert_eq!(
        result.token_usage,
        vv_agent::runtime::token_usage::summarize_task_token_usage(&result.cycles)
    );
    assert_eq!(result.token_usage.cycles.len(), 2);
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
