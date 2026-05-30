use super::*;

#[test]
fn sdk_client_run_agent_with_request_passes_shared_state() {
    let custom_tool = "_inspect_shared_state";
    let builder: LlmBuilder = Arc::new(move |_, backend, model, _| {
        let llm: Arc<dyn LlmClient> = Arc::new(ScriptedLlmClient::new(vec![
            LLMResponse::with_tool_calls(
                "inspect shared state",
                vec![ToolCall::new(custom_tool, custom_tool, BTreeMap::new())],
            ),
            LLMResponse::with_tool_calls(
                "finish",
                vec![ToolCall::new(
                    "finish_call",
                    "task_finish",
                    BTreeMap::from([("message".to_string(), json!("done"))]),
                )],
            ),
        ]));
        Ok((
            llm,
            ResolvedModelConfig::new(
                backend.to_string(),
                model.to_string(),
                model.to_string(),
                model.to_string(),
                Vec::new(),
            ),
        ))
    });
    let factory: ToolRegistryFactory = Arc::new(move || {
        let mut registry = build_default_registry();
        registry
            .register_tool(
                custom_tool,
                "Inspect SDK request shared state.",
                Arc::new(|context, _arguments| {
                    let seed = context
                        .shared_state
                        .get("seed")
                        .cloned()
                        .unwrap_or(json!(null));
                    context.shared_state.insert("seen_seed".to_string(), seed);
                    ToolExecutionResult::success("", json!({"ok": true}).to_string())
                }),
            )
            .expect("register custom tool");
        registry
    });
    let mut client = AgentSDKClient::new(AgentSDKOptions {
        auto_discover_resources: false,
        llm_builder: Some(builder),
        tool_registry_factory: Some(factory),
        ..AgentSDKOptions::default()
    });
    let mut agent = AgentDefinition::default_for_model("demo-model");
    agent.extra_tool_names.push(custom_tool.to_string());
    client
        .register_agent("demo", agent)
        .expect("register demo agent");
    let mut request = AgentSessionRunRequest::new("inspect shared state");
    request
        .shared_state
        .insert("seed".to_string(), json!("from-request"));

    let run = client
        .run_agent_with_request("demo", request)
        .expect("run with request");

    assert_eq!(run.result.final_answer.as_deref(), Some("done"));
    assert_eq!(run.result.shared_state["seen_seed"], json!("from-request"));
}

#[test]
fn sdk_default_run_request_passes_agent_one_shot_runtime_controls() {
    let captured_messages = Arc::new(Mutex::new(Vec::<Vec<Message>>::new()));
    let captured_events = Arc::new(Mutex::new(Vec::<String>::new()));
    let builder: LlmBuilder = {
        let captured_messages = Arc::clone(&captured_messages);
        Arc::new(move |_settings_path, backend, model, _timeout_seconds| {
            let llm: Arc<dyn LlmClient> = Arc::new(CapturingLlmClient {
                captured_messages: Arc::clone(&captured_messages),
            });
            Ok((
                llm,
                ResolvedModelConfig::new(
                    backend.to_string(),
                    model.to_string(),
                    model.to_string(),
                    model.to_string(),
                    Vec::new(),
                ),
            ))
        })
    };
    let client = AgentSDKClient::new_with_agent(
        AgentSDKOptions {
            auto_discover_resources: false,
            llm_builder: Some(builder),
            ..AgentSDKOptions::default()
        },
        {
            let mut agent = AgentDefinition::default_for_model("demo-model");
            agent.no_tool_policy = NoToolPolicy::Finish;
            agent
        },
    );
    let mut request = AgentSessionRunRequest::new("default request");
    request.initial_messages = vec![Message::assistant("previous assistant context")];
    request
        .shared_state
        .insert("seed".to_string(), json!("from-default-request"));
    let event_sink = Arc::clone(&captured_events);
    request.runtime_event_handler = Some(Arc::new(move |event, _payload| {
        event_sink
            .lock()
            .expect("event sink")
            .push(event.to_string());
    }));

    let run = client
        .run_with_request(request)
        .expect("run default agent with rich request");

    assert_eq!(run.agent_name, "default");
    assert_eq!(run.result.status, AgentStatus::Completed);
    assert_eq!(
        run.result.shared_state["seed"],
        json!("from-default-request")
    );
    let captured = captured_messages.lock().expect("captured messages");
    assert!(captured[0]
        .iter()
        .any(|message| message.content == "previous assistant context"));
    assert!(captured_events
        .lock()
        .expect("captured events")
        .iter()
        .any(|event| event == "run_started"));
}

#[test]
fn sdk_default_query_request_can_return_non_completed_wait_reason() {
    let builder: LlmBuilder = Arc::new(move |_settings_path, backend, model, _timeout_seconds| {
        let llm: Arc<dyn LlmClient> =
            Arc::new(ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
                "need input",
                vec![ToolCall::new(
                    "ask",
                    "ask_user",
                    BTreeMap::from([("question".to_string(), json!("pick one"))]),
                )],
            )]));
        Ok((
            llm,
            ResolvedModelConfig::new(
                backend.to_string(),
                model.to_string(),
                model.to_string(),
                model.to_string(),
                Vec::new(),
            ),
        ))
    });
    let client = AgentSDKClient::new_with_agent(
        AgentSDKOptions {
            auto_discover_resources: false,
            llm_builder: Some(builder),
            ..AgentSDKOptions::default()
        },
        AgentDefinition::default_for_model("demo-model"),
    );
    let mut request = AgentSessionRunRequest::new("ask for input");
    request
        .shared_state
        .insert("seed".to_string(), json!("from-query-request"));

    let text = client
        .query_with_request(request, false)
        .expect("non-strict query request");

    assert!(text.contains("pick one"));
}

#[test]
fn sdk_module_level_run_request_helper_passes_shared_state() {
    let custom_tool = "_module_inspect_shared_state";
    let builder: LlmBuilder = Arc::new(move |_settings_path, backend, model, _timeout_seconds| {
        let llm: Arc<dyn LlmClient> = Arc::new(ScriptedLlmClient::new(vec![
            LLMResponse::with_tool_calls(
                "inspect shared state",
                vec![ToolCall::new(custom_tool, custom_tool, BTreeMap::new())],
            ),
            LLMResponse::with_tool_calls(
                "finish",
                vec![ToolCall::new(
                    "finish",
                    "task_finish",
                    BTreeMap::from([("message".to_string(), json!("module-request"))]),
                )],
            ),
        ]));
        Ok((
            llm,
            ResolvedModelConfig::new(
                backend.to_string(),
                model.to_string(),
                model.to_string(),
                model.to_string(),
                Vec::new(),
            ),
        ))
    });
    let factory: ToolRegistryFactory = Arc::new(move || {
        let mut registry = build_default_registry();
        registry
            .register_tool(
                custom_tool,
                "Inspect module-level request shared state.",
                Arc::new(|context, _arguments| {
                    let seed = context
                        .shared_state
                        .get("seed")
                        .cloned()
                        .unwrap_or(json!(null));
                    context.shared_state.insert("seen_seed".to_string(), seed);
                    ToolExecutionResult::success("", json!({"ok": true}).to_string())
                }),
            )
            .expect("register custom tool");
        registry
    });
    let mut agent = AgentDefinition::default_for_model("demo-model");
    agent.extra_tool_names.push(custom_tool.to_string());
    let mut request = AgentSessionRunRequest::new("inspect shared state");
    request
        .shared_state
        .insert("seed".to_string(), json!("from-module-helper"));

    let run = run_with_options_and_agent_request(
        AgentSDKOptions {
            auto_discover_resources: false,
            llm_builder: Some(builder),
            tool_registry_factory: Some(factory),
            ..AgentSDKOptions::default()
        },
        agent,
        request,
    )
    .expect("module-level request helper");

    assert_eq!(run.agent_name, "inline");
    assert_eq!(run.result.final_answer.as_deref(), Some("module-request"));
    assert_eq!(
        run.result.shared_state["seen_seed"],
        json!("from-module-helper")
    );
}

#[test]
fn sdk_client_run_agent_with_request_passes_before_cycle_messages() {
    let captured_messages = Arc::new(Mutex::new(Vec::<Vec<Message>>::new()));
    let builder: LlmBuilder = {
        let captured_messages = Arc::clone(&captured_messages);
        Arc::new(move |_settings_path, backend, model, _timeout_seconds| {
            let llm: Arc<dyn LlmClient> = Arc::new(CapturingLlmClient {
                captured_messages: Arc::clone(&captured_messages),
            });
            Ok((
                llm,
                ResolvedModelConfig::new(
                    backend.to_string(),
                    model.to_string(),
                    model.to_string(),
                    model.to_string(),
                    Vec::new(),
                ),
            ))
        })
    };
    let mut client = AgentSDKClient::new(AgentSDKOptions {
        auto_discover_resources: false,
        llm_builder: Some(builder),
        ..AgentSDKOptions::default()
    });
    let mut agent = AgentDefinition::default_for_model("demo-model");
    agent.no_tool_policy = NoToolPolicy::Finish;
    client
        .register_agent("demo", agent)
        .expect("register demo agent");
    let mut request = AgentSessionRunRequest::new("start");
    request.before_cycle_messages = Some(Arc::new(|cycle_index, messages, shared_state| {
        assert_eq!(cycle_index, 1);
        assert_eq!(messages.len(), 2);
        assert!(shared_state.contains_key("todo_list"));
        vec![Message::user("sdk injected before cycle")]
    }));

    let run = client
        .run_agent_with_request("demo", request)
        .expect("run with before-cycle provider");

    assert_eq!(run.result.status, AgentStatus::Completed);
    let captured = captured_messages.lock().expect("captured messages");
    assert!(captured[0]
        .iter()
        .any(|message| message.content == "sdk injected before cycle"));
}

#[test]
fn sdk_client_run_agent_with_request_passes_interruption_messages() {
    let custom_tool = "_sdk_noop";
    let builder: LlmBuilder = Arc::new(move |_, backend, model, _| {
        let llm: Arc<dyn LlmClient> = Arc::new(ScriptedLlmClient::new(vec![
            LLMResponse::with_tool_calls(
                "two tools",
                vec![
                    ToolCall::new("noop-1", custom_tool, BTreeMap::new()),
                    ToolCall::new("noop-2", custom_tool, BTreeMap::new()),
                ],
            ),
            LLMResponse::with_tool_calls(
                "finish",
                vec![ToolCall::new(
                    "finish_after_interruption",
                    "task_finish",
                    BTreeMap::from([("message".to_string(), json!("saw interruption"))]),
                )],
            ),
        ]));
        Ok((
            llm,
            ResolvedModelConfig::new(
                backend.to_string(),
                model.to_string(),
                model.to_string(),
                model.to_string(),
                Vec::new(),
            ),
        ))
    });
    let factory: ToolRegistryFactory = Arc::new(move || {
        let mut registry = build_default_registry();
        registry
            .register_tool(
                custom_tool,
                "SDK no-op tool.",
                Arc::new(|_context, _arguments| {
                    let mut result = ToolExecutionResult::success("", "{}");
                    result.directive = ToolDirective::Continue;
                    result
                }),
            )
            .expect("register custom tool");
        registry
    });
    let mut client = AgentSDKClient::new(AgentSDKOptions {
        auto_discover_resources: false,
        llm_builder: Some(builder),
        tool_registry_factory: Some(factory),
        ..AgentSDKOptions::default()
    });
    let mut agent = AgentDefinition::default_for_model("demo-model");
    agent.max_cycles = 4;
    agent.extra_tool_names.push(custom_tool.to_string());
    client
        .register_agent("demo", agent)
        .expect("register demo agent");
    let provider_used = Arc::new(Mutex::new(false));
    let provider_flag = Arc::clone(&provider_used);
    let mut request = AgentSessionRunRequest::new("start");
    request.interruption_messages = Some(Arc::new(move || {
        let mut used = provider_flag.lock().expect("provider flag");
        if *used {
            Vec::new()
        } else {
            *used = true;
            vec![Message::user("SDK_INTERRUPT_NOW")]
        }
    }));

    let run = client
        .run_agent_with_request("demo", request)
        .expect("run with interruption provider");

    assert_eq!(run.result.status, AgentStatus::Completed);
    assert_eq!(run.result.final_answer.as_deref(), Some("saw interruption"));
    assert_eq!(
        run.result.cycles[0].tool_results[1].error_code.as_deref(),
        Some("skipped_due_to_steering")
    );
    assert!(*provider_used.lock().expect("provider flag"));
}

#[test]
fn sdk_options_log_handler_receives_runtime_events() {
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = Arc::clone(&events);
    let builder: LlmBuilder = Arc::new(move |_, backend, model, _| {
        let llm: Arc<dyn LlmClient> =
            Arc::new(ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
                "finish",
                vec![ToolCall::new(
                    "finish_call",
                    "task_finish",
                    BTreeMap::from([("message".to_string(), json!("done"))]),
                )],
            )]));
        Ok((
            llm,
            ResolvedModelConfig::new(
                backend.to_string(),
                model.to_string(),
                model.to_string(),
                model.to_string(),
                Vec::new(),
            ),
        ))
    });
    let client = AgentSDKClient::new(AgentSDKOptions {
        auto_discover_resources: false,
        llm_builder: Some(builder),
        log_handler: Some(Arc::new(move |event, _payload| {
            sink.lock().expect("events").push(event.to_string());
        })),
        ..AgentSDKOptions::default()
    });

    let run = client
        .run_with_agent(
            AgentDefinition::default_for_model("demo-model"),
            "finish through log handler",
        )
        .expect("run through SDK log handler");

    assert_eq!(run.result.status, AgentStatus::Completed);
    let events = events.lock().expect("events");
    assert!(events.iter().any(|event| event == "run_started"));
    assert!(events.iter().any(|event| event == "run_completed"));
}

#[test]
fn sdk_options_runtime_hooks_patch_llm_response() {
    let builder: LlmBuilder = Arc::new(move |_settings_path, backend, model, _timeout_seconds| {
        let llm: Arc<dyn LlmClient> = Arc::new(ScriptedLlmClient::new(vec![LLMResponse::new(
            "plain response",
        )]));
        Ok((
            llm,
            ResolvedModelConfig::new(
                backend.to_string(),
                model.to_string(),
                model.to_string(),
                model.to_string(),
                Vec::new(),
            ),
        ))
    });
    let client = AgentSDKClient::new(AgentSDKOptions {
        auto_discover_resources: false,
        llm_builder: Some(builder),
        runtime_hooks: vec![Arc::new(ForceFinishHook)],
        ..AgentSDKOptions::default()
    });

    let run = client
        .run_with_agent(
            AgentDefinition::default_for_model("demo-model"),
            "use sdk hook",
        )
        .expect("run through hook");

    assert_eq!(run.result.status, AgentStatus::Completed);
    assert_eq!(run.result.final_answer.as_deref(), Some("hook-finish"));
}

#[test]
fn sdk_options_runtime_hooks_apply_to_injected_runtime() {
    let runtime = AgentRuntime::new(ScriptedLlmClient::new(vec![LLMResponse::new(
        "plain response",
    )]));
    let client = AgentSDKClient::new(AgentSDKOptions {
        auto_discover_resources: false,
        runtime_hooks: vec![Arc::new(ForceFinishHook)],
        ..AgentSDKOptions::default()
    })
    .with_runtime(runtime);

    let run = client
        .run_with_agent(
            AgentDefinition::default_for_model("demo-model"),
            "use injected runtime hook",
        )
        .expect("run through injected hook");

    assert_eq!(run.result.status, AgentStatus::Completed);
    assert_eq!(run.result.final_answer.as_deref(), Some("hook-finish"));
}

#[test]
fn sdk_options_pass_debug_dump_dir_to_custom_llm_builder() {
    #[derive(Clone, Default)]
    struct DebugAwareLlm {
        debug_dump_dir: Arc<Mutex<Option<std::path::PathBuf>>>,
    }

    impl LlmClient for DebugAwareLlm {
        fn complete(&self, _request: LlmRequest) -> Result<LLMResponse, LlmError> {
            Ok(LLMResponse::with_tool_calls(
                "done",
                vec![ToolCall::new(
                    "finish",
                    "task_finish",
                    BTreeMap::from([("message".to_string(), json!("ok"))]),
                )],
            ))
        }

        fn set_debug_dump_dir(&self, debug_dump_dir: &std::path::Path) {
            *self.debug_dump_dir.lock().expect("debug dir") = Some(debug_dump_dir.to_path_buf());
        }
    }

    let workspace = tempfile::tempdir().expect("workspace");
    let dump_dir = workspace.path().join("llm-dumps");
    let llm = DebugAwareLlm::default();
    let observed_debug_dump_dir = Arc::clone(&llm.debug_dump_dir);
    let llm_builder: LlmBuilder = Arc::new(move |_settings, _backend, _model, _timeout| {
        Ok((
            Arc::new(llm.clone()) as Arc<dyn LlmClient>,
            ResolvedModelConfig::new(
                "deepseek",
                "deepseek-v4-pro",
                "deepseek-v4-pro",
                "deepseek-v4-pro",
                Vec::new(),
            ),
        ))
    });
    let client = AgentSDKClient::new(AgentSDKOptions {
        workspace: workspace.path().to_path_buf(),
        debug_dump_dir: Some(dump_dir.to_string_lossy().into_owned()),
        llm_builder: Some(llm_builder),
        tool_registry_factory: Some(Arc::new(build_default_registry)),
        auto_discover_resources: false,
        ..AgentSDKOptions::default()
    });

    let mut agent = AgentDefinition::default_for_model("deepseek-v4-pro");
    agent.backend = Some("deepseek".to_string());
    let run = client.run_with_agent(agent, "say ok").expect("run");

    assert_eq!(run.result.status, AgentStatus::Completed);
    assert_eq!(
        *observed_debug_dump_dir.lock().expect("debug dir"),
        Some(dump_dir)
    );
}

#[test]
fn sdk_client_builds_agent_system_prompt_from_agent_definition() {
    let captured_messages = Arc::new(Mutex::new(Vec::<Vec<Message>>::new()));
    let builder: LlmBuilder = {
        let captured_messages = Arc::clone(&captured_messages);
        Arc::new(move |_settings_path, backend, model, _timeout_seconds| {
            let llm: Arc<dyn LlmClient> = Arc::new(CapturingLlmClient {
                captured_messages: Arc::clone(&captured_messages),
            });
            Ok((
                llm,
                ResolvedModelConfig::new(
                    backend.to_string(),
                    model.to_string(),
                    model.to_string(),
                    model.to_string(),
                    Vec::new(),
                ),
            ))
        })
    };
    let client = AgentSDKClient::new(AgentSDKOptions {
        auto_discover_resources: false,
        llm_builder: Some(builder),
        ..AgentSDKOptions::default()
    });
    let mut agent = AgentDefinition::default_for_model("demo-model");
    agent.description = "Research profile must inspect files before answering.".to_string();
    agent.no_tool_policy = NoToolPolicy::Finish;
    agent.allow_interruption = false;
    agent.use_workspace = false;
    agent.enable_todo_management = false;

    let run = client
        .run_with_agent(agent, "capture prompt")
        .expect("run through capturing builder");

    assert_eq!(run.result.status, AgentStatus::Completed);
    let captured = captured_messages.lock().expect("captured messages");
    let system_message = captured[0]
        .iter()
        .find(|message| message.role == MessageRole::System)
        .expect("system message");
    assert!(system_message.content.contains("<Agent Definition>"));
    assert!(system_message
        .content
        .contains("Research profile must inspect files before answering."));
    assert!(system_message.content.contains("<Tools>"));
    assert!(system_message.content.contains("task_finish"));
}

#[test]
fn sdk_client_applies_resolved_vv_llm_token_limits_to_runtime_memory() {
    let captured_messages = Arc::new(Mutex::new(Vec::<Vec<Message>>::new()));
    let builder: LlmBuilder = {
        let captured_messages = Arc::clone(&captured_messages);
        Arc::new(move |_settings_path, backend, model, _timeout_seconds| {
            let llm: Arc<dyn LlmClient> = Arc::new(MemoryLimitInspectingLlmClient {
                captured_messages: Arc::clone(&captured_messages),
            });
            Ok((
                llm,
                ResolvedModelConfig::new(
                    backend.to_string(),
                    model.to_string(),
                    model.to_string(),
                    format!("{model}-resolved"),
                    Vec::new(),
                )
                .with_token_limits(Some(80), Some(10)),
            ))
        })
    };
    let client = AgentSDKClient::new(AgentSDKOptions {
        auto_discover_resources: false,
        llm_builder: Some(builder),
        ..AgentSDKOptions::default()
    });
    let mut agent = AgentDefinition::default_for_model("demo-model");
    agent.no_tool_policy = NoToolPolicy::Continue;
    agent.max_cycles = 2;
    agent.use_workspace = false;
    agent
        .metadata
        .insert("autocompact_buffer_tokens".to_string(), json!(0));

    let run = client
        .run_with_agent(agent, "keep context compact")
        .expect("run through memory limit client");

    assert_eq!(run.result.status, AgentStatus::Completed);
    let captured = captured_messages.lock().expect("captured messages");
    let second_request = captured.get(1).expect("second request");
    assert!(
        second_request
            .iter()
            .any(|message| message.content.contains("<Compressed Agent Memory>")),
        "SDK-built runtime did not use resolved vv-llm token limits for compaction: {second_request:#?}"
    );
}

struct ForceFinishHook;

impl RuntimeHook for ForceFinishHook {
    fn after_llm(&self, event: AfterLlmEvent<'_>) -> Option<LLMResponse> {
        Some(LLMResponse::with_tool_calls(
            event.response.content.clone(),
            vec![ToolCall::new(
                "hook-finish",
                "task_finish",
                BTreeMap::from([("message".to_string(), json!("hook-finish"))]),
            )],
        ))
    }
}

#[derive(Clone)]
struct CapturingLlmClient {
    captured_messages: Arc<Mutex<Vec<Vec<Message>>>>,
}

impl LlmClient for CapturingLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        self.captured_messages
            .lock()
            .expect("captured messages")
            .push(request.messages);
        Ok(LLMResponse::new("captured answer"))
    }
}

#[derive(Clone)]
struct MemoryLimitInspectingLlmClient {
    captured_messages: Arc<Mutex<Vec<Vec<Message>>>>,
}

impl LlmClient for MemoryLimitInspectingLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let request_index = {
            let mut captured = self.captured_messages.lock().expect("captured messages");
            captured.push(request.messages);
            captured.len()
        };
        if request_index == 1 {
            return Ok(LLMResponse::new("large assistant context ".repeat(80)));
        }
        Ok(LLMResponse::with_tool_calls(
            "finish",
            vec![ToolCall::new(
                "finish_after_compaction",
                "task_finish",
                BTreeMap::from([("message".to_string(), json!("compacted"))]),
            )],
        ))
    }
}
