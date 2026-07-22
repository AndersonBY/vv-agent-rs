use super::*;

#[test]
fn real_configured_sub_agent_producer_matches_locked_event_fixture_line_by_line() {
    let mapped_events = Arc::new(Mutex::new(Vec::new()));
    let mapped_events_for_handler = mapped_events.clone();
    let outcomes = Arc::new(Mutex::new(BTreeMap::<String, SubTaskOutcome>::new()));
    let event_handler: vv_agent::RunEventHandler = Arc::new(move |run_event| {
        if matches!(
            run_event.payload(),
            RunEventPayload::SubRunStarted { .. } | RunEventPayload::SubRunCompleted { .. }
        ) {
            mapped_events_for_handler
                .lock()
                .expect("mapped contract events")
                .push(run_event.clone());
        }
    });
    let controls = RuntimeRunControls {
        event_handler: Some(event_handler),
        execution_context: Some(ExecutionContext {
            metadata: BTreeMap::from([
                ("_vv_agent_run_id".to_string(), json!("execution-run")),
                ("_vv_agent_trace_id".to_string(), json!("trace-parity")),
                ("_vv_agent_session_id".to_string(), json!("parent-session")),
            ]),
            ..ExecutionContext::default()
        }),
        run_context: Some(RunContext {
            run_id: "parent-run".to_string(),
            agent_name: "parent".to_string(),
            ..RunContext::default()
        }),
        ..RuntimeRunControls::default()
    };

    let build_registry = || {
        let mut registry = build_default_registry();
        let outcomes = outcomes.clone();
        registry
            .register(ToolSpec::new(
                "contract_delegate",
                "Run the configured sub-agent contract request.",
                Arc::new(move |context, _arguments| {
                    let runner = context
                        .sub_task_runner
                        .clone()
                        .expect("configured sub-task runner");
                    let mut request = SubTaskRequest::new("researcher", "Collect facts");
                    request.metadata = BTreeMap::from([
                        ("parent_run_id".to_string(), json!("parent-run")),
                        (
                            "parent_tool_call_id".to_string(),
                            json!(context.tool_call_id),
                        ),
                    ]);
                    let outcome = runner(request);
                    outcomes
                        .lock()
                        .expect("contract outcomes")
                        .insert(context.tool_call_id.clone(), outcome.clone());
                    ToolExecutionResult::success(
                        "",
                        serde_json::to_string(&outcome).expect("serialize contract outcome"),
                    )
                }),
            ))
            .expect("register contract delegate");
        registry
    };
    let build_parent = |sub_agent: SubAgentConfig| {
        let mut parent =
            AgentTask::new("parent-task", "child-model", "Parent prompt", "Parent task");
        parent.max_cycles = 3;
        parent.extra_tool_names = vec!["contract_delegate".to_string()];
        parent
            .sub_agents
            .insert("researcher".to_string(), sub_agent);
        parent
    };

    let success_llm = ScriptedLlmClient::from_steps(vec![
        ScriptStep::response(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "delegate",
                "contract_delegate",
                json!({}),
            )],
        )),
        ScriptStep::response(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "child-finish",
                "task_finish",
                json!({"message": "child done"}),
            )],
        )),
        ScriptStep::response(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "parent-finish",
                "task_finish",
                json!({"message": "parent done"}),
            )],
        )),
    ]);
    let success = AgentRuntime::new(success_llm)
        .with_tool_registry(build_registry())
        .run_with_controls(
            build_parent(SubAgentConfig::new("child-model", "Research")),
            controls.clone(),
        )
        .expect("successful configured sub-agent producer");
    assert_eq!(success.status, AgentStatus::Completed);

    let failure_llm = ScriptedLlmClient::from_steps(vec![
        ScriptStep::response(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "delegate-failed",
                "contract_delegate",
                json!({}),
            )],
        )),
        ScriptStep::response(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "parent-finish-failed",
                "task_finish",
                json!({"message": "parent handled child failure"}),
            )],
        )),
    ]);
    let mut invalid_sub_agent = SubAgentConfig::new("child-model", "Research");
    invalid_sub_agent.system_prompt = Some(" \n ".to_string());
    let failure = AgentRuntime::new(failure_llm)
        .with_tool_registry(build_registry())
        .run_with_controls(build_parent(invalid_sub_agent), controls)
        .expect("runtime-invalid configured sub-agent producer");
    assert_eq!(failure.status, AgentStatus::Completed);

    let mapped_events = mapped_events.lock().expect("mapped contract events");
    assert_eq!(mapped_events.len(), 4);
    assert_eq!(
        mapped_events
            .iter()
            .map(|event| event.event_id().as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        4
    );
    assert!(mapped_events
        .iter()
        .all(|event| event.event_id().as_str().starts_with("evt_")));
    assert!(mapped_events.iter().all(|event| event.created_at() > 0.0));
    assert!(mapped_events
        .windows(2)
        .all(|pair| pair[0].created_at() <= pair[1].created_at()));
    assert_eq!(mapped_events[0].run_id(), mapped_events[1].run_id());
    assert_eq!(mapped_events[2].run_id(), mapped_events[3].run_id());
    assert_ne!(mapped_events[0].run_id(), mapped_events[2].run_id());
    assert_ne!(mapped_events[0].run_id(), "parent-run");

    let raw_events = mapped_events
        .iter()
        .map(|event| serde_json::to_value(event).expect("serialize raw contract event"))
        .collect::<Vec<_>>();
    let outcomes = outcomes.lock().expect("contract outcomes");
    for pair in raw_events.chunks_exact(2) {
        let tool_call_id = pair[0]["parent_tool_call_id"]
            .as_str()
            .expect("parent tool call id");
        let outcome = outcomes
            .get(tool_call_id)
            .expect("matching sub-task outcome");
        assert_eq!(
            pair[0]["parent_tool_call_id"],
            pair[1]["parent_tool_call_id"]
        );
        assert_eq!(pair[0]["task_id"], json!(outcome.task_id));
        assert_eq!(pair[1]["task_id"], json!(outcome.task_id));
        assert_eq!(pair[0]["session_id"], json!(outcome.session_id));
        assert_eq!(pair[1]["session_id"], json!(outcome.session_id));
        assert_eq!(pair[0]["child_session_id"], pair[0]["session_id"]);
        assert_eq!(pair[1]["child_session_id"], pair[1]["session_id"]);
    }

    let actual = raw_events
        .into_iter()
        .map(|event| {
            let mut value = event;
            let failed_pair = value["parent_tool_call_id"] == "delegate-failed";
            let task_id = if failed_pair {
                "child-task-failed"
            } else {
                "child-task"
            };
            let session_id = if failed_pair {
                "child-session-failed"
            } else {
                "child-session"
            };
            value["event_id"] = json!("evt_dynamic");
            value["run_id"] = json!("run_dynamic");
            value["session_id"] = json!(session_id);
            value["child_session_id"] = json!(session_id);
            value["task_id"] = json!(task_id);
            value["created_at"] = json!(0.0);
            value
        })
        .collect::<Vec<_>>();
    let expected = CONFIGURED_SUB_AGENT_EVENTS_FIXTURE
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Value>(line).expect("event fixture line"))
        .collect::<Vec<_>>();
    let fixture = contract();
    assert!(expected
        .iter()
        .all(|event| event["version"] == fixture["version"]));
    assert_eq!(actual.len(), expected.len());
    for (line_index, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
        assert_eq!(actual, expected, "event fixture line {}", line_index + 1);
    }
    let lifecycle = &fixture["lifecycle"];
    let expected_pair = lifecycle["event_sequence"]
        .as_array()
        .expect("lifecycle event sequence");
    for pair in actual.chunks_exact(2) {
        assert_eq!(pair[0]["type"], expected_pair[0]);
        assert_eq!(pair[1]["type"], expected_pair[1]);
        assert_eq!(pair[0]["run_id"], pair[1]["run_id"]);
    }
    assert_eq!(
        actual[3]["metadata"]["error_code"].is_string(),
        lifecycle["failure_error_code_in_metadata"]
    );
    assert_eq!(
        actual[1]["token_usage"]["total_tokens"].is_null(),
        lifecycle["preserve_successful_missing_token_usage"]
    );
    assert_eq!(
        actual[3].get("token_usage").is_none(),
        lifecycle["omit_token_usage_when_unavailable"]
    );
}
