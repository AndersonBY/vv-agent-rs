use super::*;

#[test]
fn memory_capacity_defaults_match_contract_without_rewriting_explicit_values() {
    let contract = contract();
    let expected = &contract["capacity_contract"];

    assert_eq!(
        AgentTask::new("task", "model", "system", "user").memory_compact_threshold,
        expected["configured_default_threshold"].as_u64().unwrap()
    );
    assert_eq!(
        MemoryManagerConfig::default().compact_threshold,
        expected["configured_default_threshold"].as_u64().unwrap()
    );

    let mut explicit = AgentTask::new("task", "model", "system", "user");
    explicit.memory_compact_threshold = 128_000;
    let restored: AgentTask =
        serde_json::from_value(serde_json::to_value(&explicit).expect("serialize task"))
            .expect("restore task");
    assert_eq!(restored.memory_compact_threshold, 128_000);
}

#[test]
fn runtime_context_window_resolution_matches_contract_cases_and_zero_capability() {
    let contract = contract();
    let mut cases = contract["capacity_contract"]["context_window_resolution"]["cases"]
        .as_array()
        .expect("context resolution cases")
        .clone();
    cases.push(json!({
        "name": "zero_resolved_capability_uses_derived_planning_context",
        "input": {
            "task_metadata_model_context_window": 0,
            "resolved_model_context_window": 0
        },
        "expected_model_context_window": contract["capacity_contract"]["unknown_context_window_strategy"]["default_model_context_window"]
    }));

    for case in &cases {
        let input = &case["input"];
        let workspace = tempfile::tempdir().expect("capacity workspace");
        let mut runtime = AgentRuntime::new(PromptTooLongThenSuccess::new(1));
        if let Some(context_length) = input["resolved_model_context_window"].as_u64() {
            let settings_file = workspace.path().join("llm_settings.json");
            std::fs::write(
                &settings_file,
                json!({
                    "VERSION": "2",
                    "endpoints": [{
                        "id": "deepseek-primary",
                        "api_key": "sk-test",
                        "api_base": "https://api.deepseek.test"
                    }],
                    "backends": {
                        "deepseek": {
                            "models": {
                                "deepseek-v4-pro": {
                                    "id": "deepseek-v4-pro",
                                    "endpoints": ["deepseek-primary"],
                                    "context_length": context_length,
                                    "max_output_tokens": 32_000
                                }
                            }
                        }
                    }
                })
                .to_string(),
            )
            .expect("capacity settings");
            runtime = runtime
                .with_settings_file(settings_file)
                .with_default_backend("deepseek");
        }

        let mut task = AgentTask::new(
            format!("context-resolution-{}", case["name"].as_str().unwrap()),
            "deepseek-v4-pro",
            "system",
            "continue",
        );
        task.max_cycles = 1;
        task.no_tool_policy = NoToolPolicy::Finish;
        task.metadata.insert(
            "model_context_window".to_string(),
            input["task_metadata_model_context_window"].clone(),
        );
        let logs = Arc::new(Mutex::new(Vec::<RunEvent>::new()));
        let log_sink = logs.clone();

        runtime
            .run_with_controls(
                task,
                RuntimeRunControls {
                    event_handler: Some(Arc::new(move |event| {
                        if matches!(
                            event.payload(),
                            RunEventPayload::MemoryCompactStarted { .. }
                        ) {
                            log_sink
                                .lock()
                                .expect("context resolution logs")
                                .push(event.clone());
                        }
                    })),
                    ..RuntimeRunControls::default()
                },
            )
            .unwrap_or_else(|error| panic!("{}: {error}", case["name"]));

        let logs = logs.lock().expect("context resolution logs");
        let started = logs
            .iter()
            .find(|event| {
                matches!(
                    event.payload(),
                    RunEventPayload::MemoryCompactStarted {
                        trigger: MemoryCompactTrigger::PromptTooLong,
                        ..
                    }
                )
            })
            .unwrap_or_else(|| panic!("{}: prompt-too-long event", case["name"]));
        let RunEventPayload::MemoryCompactStarted {
            model_context_window,
            ..
        } = started.payload()
        else {
            unreachable!("matched memory compact started")
        };
        assert_eq!(
            Value::from(*model_context_window),
            case["expected_model_context_window"],
            "{}",
            case["name"]
        );
    }
}

#[test]
fn runtime_capacity_resolution_matches_every_contract_case() {
    let contract = contract();
    let capacity = &contract["capacity_contract"];

    for case in capacity["cases"].as_array().expect("capacity cases") {
        let input = &case["input"];
        let expected = &case["expected"];
        let mut task = AgentTask::new(
            format!("capacity-{}", case["name"].as_str().unwrap()),
            "capacity-model",
            "system",
            "continue",
        );
        task.initial_messages = vec![
            Message::system("system"),
            Message::user("first"),
            Message::assistant("working"),
        ];
        task.max_cycles = 1;
        task.no_tool_policy = NoToolPolicy::Finish;
        task.memory_compact_threshold = input["configured_threshold"].as_u64().unwrap();
        task.metadata.insert(
            "model_context_window".to_string(),
            input["model_context_window"].clone(),
        );
        task.metadata.insert(
            "autocompact_buffer_tokens".to_string(),
            input["autocompact_buffer_tokens"].clone(),
        );
        if let Some(value) = input["task_metadata_reserved_output_tokens"].as_u64() {
            task.metadata
                .insert("reserved_output_tokens".to_string(), Value::from(value));
        }
        if let Some(value) = input["model_max_output_tokens"].as_u64() {
            task.metadata
                .insert("model_max_output_tokens".to_string(), Value::from(value));
        }
        if let Some(value) = input["effective_model_max_tokens"].as_u64() {
            task.model_settings = Some(
                ModelSettings::builder()
                    .max_tokens(u32::try_from(value).expect("request limit fits u32"))
                    .build(),
            );
        }

        let logs = Arc::new(Mutex::new(Vec::<RunEvent>::new()));
        let log_sink = logs.clone();
        AgentRuntime::new(PromptTooLongThenSuccess::new(1))
            .run_with_controls(
                task,
                RuntimeRunControls {
                    event_handler: Some(Arc::new(move |event| {
                        if matches!(
                            event.payload(),
                            RunEventPayload::MemoryCompactStarted { .. }
                                | RunEventPayload::MemoryCompactCompleted { .. }
                        ) {
                            log_sink.lock().expect("capacity logs").push(event.clone());
                        }
                    })),
                    ..RuntimeRunControls::default()
                },
            )
            .unwrap_or_else(|error| panic!("{}: {error}", case["name"]));

        let logs = logs.lock().expect("capacity logs");
        let started = logs
            .iter()
            .find(|event| {
                matches!(
                    event.payload(),
                    RunEventPayload::MemoryCompactStarted {
                        trigger: MemoryCompactTrigger::PromptTooLong,
                        ..
                    }
                )
            })
            .unwrap_or_else(|| panic!("{}: forced started event", case["name"]));
        let started = serde_json::to_value(started).expect("capacity started event wire");
        assert_eq!(
            started["configured_threshold"],
            input["configured_threshold"]
        );
        assert_eq!(
            started["effective_threshold"],
            expected["effective_threshold"]
        );
        assert_eq!(
            started["microcompact_threshold"],
            expected["microcompact_threshold"]
        );
        assert_eq!(
            started["model_context_window"],
            input["model_context_window"]
        );
        assert_eq!(
            started["model_max_output_tokens"],
            input["model_max_output_tokens"]
        );
        assert_eq!(
            started["reserved_output_tokens"],
            expected["reserved_output_tokens"]
        );
        assert_eq!(
            started["reserved_output_source"],
            expected["reserved_output_source"]
        );
        assert_eq!(
            started["autocompact_buffer_tokens"],
            input["autocompact_buffer_tokens"]
        );
    }
}

fn simultaneous_warning_microcompact_messages(recent_tool_chars: usize) -> Vec<Message> {
    vec![
        Message::system("system"),
        Message::user("start"),
        Message {
            tool_calls: vec![ToolCall::new("call_old", "read_file", BTreeMap::new())],
            ..Message::assistant("old tool call")
        },
        Message::tool("x".repeat(800), "call_old"),
        Message {
            tool_calls: vec![ToolCall::new("call_recent", "read_file", BTreeMap::new())],
            ..Message::assistant("recent tool call")
        },
        Message::tool("y".repeat(recent_tool_chars), "call_recent"),
    ]
}

#[test]
fn warning_is_evaluated_from_post_microcompact_usage_on_both_threshold_paths() {
    let contract = contract();
    assert_eq!(
        contract["compaction_events"]["simultaneous_warning_and_microcompact"]["order"],
        json!([
            "microcompact_eligible_old_tool_results",
            "recalculate_effective_length",
            "append_memory_warning_only_if_post_microcompact_length_remains_eligible"
        ])
    );

    for initial_usage in [3_800, 4_200] {
        let mut manager = MemoryManager::new(MemoryManagerConfig {
            compact_threshold: 4_000,
            model_context_window: 4_000,
            reserved_output_tokens: 0,
            autocompact_buffer_tokens: 0,
            warning_threshold_percentage: 90,
            include_memory_warning: true,
            language: "en-US".to_string(),
            microcompact_keep_recent_cycles: 1,
            microcompact_min_result_length: 500,
            ..MemoryManagerConfig::default()
        });

        let (compacted, changed) = manager.compact_for_cycle_with_usage(
            &simultaneous_warning_microcompact_messages(800),
            4,
            false,
            Some(initial_usage),
            None,
        );

        assert!(changed, "initial usage {initial_usage}");
        assert!(
            compacted
                .iter()
                .any(|message| message.content == CLEARED_MARKER),
            "initial usage {initial_usage}"
        );
        assert!(
            compacted.iter().all(|message| !message
                .content
                .contains("current memory usage has exceeded")),
            "warning used the stale pre-microcompact length for {initial_usage}"
        );
        assert!(
            compacted
                .iter()
                .all(|message| !message.content.contains("<Compressed Agent Memory>")),
            "microcompact should avoid summary for {initial_usage}"
        );
    }
}

#[test]
fn warning_is_retained_when_post_microcompact_usage_remains_eligible() {
    let messages = simultaneous_warning_microcompact_messages(8_000);
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        reserved_output_tokens: 0,
        autocompact_buffer_tokens: 0,
        warning_threshold_percentage: 90,
        include_memory_warning: true,
        language: "en-US".to_string(),
        microcompact_keep_recent_cycles: 1,
        microcompact_min_result_length: 500,
        ..MemoryManagerConfig::default()
    });
    let (post_microcompact, cleared) = manager.microcompact_messages(&messages, 4);
    assert_eq!(cleared, 1);
    let post_tokens = count_messages_tokens(&post_microcompact, &manager.config.model);
    assert!(post_tokens > 900);
    let full_threshold = post_tokens + 100;
    manager.config.compact_threshold = full_threshold;
    manager.config.model_context_window = full_threshold;

    let (compacted, changed) =
        manager.compact_for_cycle_with_usage(&messages, 4, false, Some(full_threshold), None);

    assert!(changed);
    assert!(compacted
        .iter()
        .any(|message| message.content == CLEARED_MARKER));
    assert!(compacted.iter().any(|message| message
        .content
        .contains("current memory usage has exceeded")));
}
