use super::*;

#[derive(Clone)]
struct ChildModelProvider {
    client: Arc<dyn LlmClient>,
}

impl ModelProvider for ChildModelProvider {
    fn resolve(&self, model: &ModelRef) -> Result<ResolvedModelConfig, ModelError> {
        assert!(model.backend_name().is_none());
        Ok(ResolvedModelConfig::new(
            "test",
            model.model(),
            model.model(),
            model.model(),
            Vec::new(),
        )
        .with_capabilities(true, true, true))
    }

    fn client(&self, _resolved: &ResolvedModelConfig) -> Result<Arc<dyn LlmClient>, ModelError> {
        Ok(self.client.clone())
    }
}

#[derive(Debug)]
struct AppMarker(&'static str);

#[test]
fn real_child_run_projects_capabilities_identity_model_and_filtered_workspace() {
    let child_request_metadata = Arc::new(Mutex::new(None));
    let child_request_metadata_for_step = child_request_metadata.clone();
    let inspected_context = Arc::new(Mutex::new(None));
    let inspected_context_for_tool = inspected_context.clone();
    let hidden_read = Arc::new(Mutex::new(None));
    let hidden_read_for_tool = hidden_read.clone();

    let shared_llm = ScriptedLlmClient::from_steps(vec![
        ScriptStep::response(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "delegate",
                "create_sub_task",
                json!({
                    "agent_id": "researcher",
                    "task_description": "Inspect child context",
                    "exclude_files_pattern": "^(?:generated|logs)/"
                }),
            )],
        )),
        ScriptStep::callback(move |request| {
            *child_request_metadata_for_step
                .lock()
                .expect("child request metadata") = Some(request.clone());
            Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "inspect",
                    "inspect_child_context",
                    json!({}),
                )],
            ))
        }),
        ScriptStep::callback(|request| {
            assert!(
                request
                    .messages
                    .iter()
                    .any(|message| message.image_url.as_deref() == Some("memory://child-image")),
                "resolved child native_multimodal capability was not applied"
            );
            Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "child-finish",
                    "task_finish",
                    json!({"message": "child done"}),
                )],
            ))
        }),
        ScriptStep::response(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "parent-finish",
                "task_finish",
                json!({"message": "parent done"}),
            )],
        )),
    ]);
    let provider: Arc<dyn ModelProvider> = Arc::new(ChildModelProvider {
        client: Arc::new(shared_llm.clone()),
    });
    let mut registry = build_default_registry();
    registry
        .register(ToolSpec::new(
            "inspect_child_context",
            "Inspect projected child capabilities.",
            Arc::new(move |context, _arguments| {
                let run = context.run_context.clone().expect("child run context");
                let files = context
                    .workspace_backend
                    .list_files(".", "**/*")
                    .expect("child workspace listing");
                *inspected_context_for_tool
                    .lock()
                    .expect("inspected context") = Some(json!({
                    "run_id": run.run_id,
                    "agent_name": run.agent_name,
                    "model": run.model.as_ref().map(ModelRef::model),
                    "metadata": run.metadata,
                    "app_state": context.app_state::<AppMarker>().map(|marker| marker.0),
                    "has_model_provider": context.model_provider.is_some(),
                    "is_filtered_backend": context
                        .workspace_backend
                        .as_any()
                        .is::<DiscoveryFilteredWorkspaceBackend>(),
                    "shared_state_has_parent_secret": context
                        .shared_state
                        .contains_key("parent_secret"),
                    "files": files,
                }));
                *hidden_read_for_tool.lock().expect("hidden read") = Some(
                    context
                        .workspace_backend
                        .read_text("generated/cache.bin")
                        .expect("known hidden path remains readable"),
                );
                ToolExecutionResult {
                    tool_call_id: String::new(),
                    content: json!({"ok": true}).to_string(),
                    status: ToolResultStatus::Success,
                    directive: ToolDirective::Continue,
                    error_code: None,
                    metadata: BTreeMap::new(),
                    image_url: Some("memory://child-image".to_string()),
                    image_path: None,
                }
            }),
        ))
        .expect("register inspection tool");
    let backend = Arc::new(MemoryWorkspaceBackend::default());
    backend
        .write_text("src/main.py", "print('ok')", false)
        .expect("write visible file");
    backend
        .write_text("generated/cache.bin", "hidden data", false)
        .expect("write hidden file");
    let manager = SubTaskManager::default();
    let mut runtime = AgentRuntime::new(shared_llm).with_tool_registry(registry);
    runtime.workspace_backend = backend;
    let mut parent = AgentTask::new("parent-task", "parent-model", "Parent prompt", "Delegate");
    parent.max_cycles = 4;
    parent.extra_tool_names = vec!["inspect_child_context".to_string()];
    parent.model_settings = Some(ModelSettings {
        temperature: Some(0.25),
        max_tokens: Some(512),
        ..ModelSettings::default()
    });
    parent
        .initial_shared_state
        .insert("parent_secret".to_string(), json!("must not leak"));
    let mut child = SubAgentConfig::new("child-model", "Research");
    child.backend = Some(
        contract()["model_resolution"]["blank_backend_input"]
            .as_str()
            .expect("blank backend input")
            .to_string(),
    );
    child.max_cycles = 3;
    parent.sub_agents.insert("researcher".to_string(), child);
    let parent_token = vv_agent::CancellationToken::default();
    let controls = RuntimeRunControls {
        cancellation_token: Some(parent_token.clone()),
        execution_context: Some(ExecutionContext {
            state_store: Some(Arc::new(InMemoryStateStore::new())),
            app_state: Some(Arc::new(AppMarker("inherited"))),
            metadata: BTreeMap::from([
                ("_vv_agent_run_id".to_string(), json!("parent-run")),
                ("_vv_agent_trace_id".to_string(), json!("trace-parity")),
                ("_vv_agent_session_id".to_string(), json!("parent-session")),
                ("_vv_agent_input".to_string(), json!("parent input")),
                (
                    "_vv_agent_trace_context".to_string(),
                    json!({"traceparent": "00-contract"}),
                ),
            ]),
            ..ExecutionContext::default()
        }),
        workspace_backend: Some(runtime.workspace_backend.clone()),
        model_provider: Some(provider),
        run_context: Some(RunContext {
            run_id: "parent-run".to_string(),
            agent_name: "parent".to_string(),
            ..RunContext::default()
        }),
        sub_task_manager: Some(manager.clone()),
        ..RuntimeRunControls::default()
    };

    let result = runtime
        .run_with_controls(parent, controls)
        .expect("parent and child run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert!(!parent_token.is_cancelled());
    let child_request = child_request_metadata
        .lock()
        .expect("child request metadata")
        .clone()
        .expect("captured child request");
    assert_eq!(child_request.model, "child-model");
    assert_eq!(
        child_request.model_settings,
        Some(ModelSettings {
            temperature: Some(0.25),
            max_tokens: Some(512),
            ..ModelSettings::default()
        })
    );
    let request_metadata = child_request
        .metadata
        .as_object()
        .expect("request metadata");
    assert_ne!(request_metadata["_vv_agent_run_id"], "parent-run");
    assert_eq!(request_metadata["_vv_agent_trace_id"], "trace-parity");
    assert_eq!(request_metadata["_vv_agent_agent_name"], "researcher");
    assert_ne!(request_metadata["_vv_agent_session_id"], "parent-session");
    assert!(!request_metadata.contains_key("_vv_agent_input"));
    assert_eq!(
        request_metadata["_vv_agent_trace_context"],
        json!({"traceparent": "00-contract"})
    );
    let inspected = inspected_context
        .lock()
        .expect("inspected context")
        .clone()
        .expect("inspection payload");
    assert_eq!(inspected["agent_name"], "researcher");
    assert_eq!(inspected["model"], "child-model");
    assert_eq!(inspected["metadata"]["is_sub_task"], true);
    assert_eq!(inspected["metadata"]["sub_agent_name"], "researcher");
    assert_eq!(inspected["metadata"]["parent_run_id"], "parent-run");
    assert_eq!(inspected["metadata"]["parent_tool_call_id"], "delegate");
    assert_eq!(inspected["metadata"]["trace_id"], "trace-parity");
    assert_eq!(inspected["app_state"], "inherited");
    assert_eq!(inspected["has_model_provider"], true);
    assert_eq!(inspected["is_filtered_backend"], true);
    assert_eq!(inspected["shared_state_has_parent_secret"], false);
    assert_eq!(inspected["files"], json!(["src/main.py"]));
    assert_eq!(
        hidden_read.lock().expect("hidden read").as_deref(),
        Some("hidden data")
    );
    let child_result_payload: Value =
        serde_json::from_str(&result.cycles[0].tool_results[0].content)
            .expect("child tool result payload");
    let child_task_id = child_result_payload["task_id"]
        .as_str()
        .expect("child task id")
        .to_string();
    let snapshot = manager.get(&child_task_id).expect("manager snapshot");
    assert_eq!(snapshot.parent_run_id.as_deref(), Some("parent-run"));
    assert_eq!(snapshot.parent_tool_call_id.as_deref(), Some("delegate"));
    assert_eq!(
        snapshot.parent_run_id.as_deref() == Some("parent-run")
            && snapshot.parent_tool_call_id.as_deref() == Some("delegate"),
        contract()["manager"]["persists_parent_lineage"]
    );
    assert_eq!(
        contract()["model_resolution"]["blank_backend_treated_as_absent"],
        true
    );
    assert_eq!(
        serde_json::to_value(&snapshot.resolved).expect("resolved metadata"),
        contract()["model_resolution"]["resolved_without_endpoint"]
    );
    let status = manager.status_entries(std::slice::from_ref(&child_task_id), "snapshot", 10);
    assert_eq!(
        status[0]["snapshot"]["workspace_files"],
        json!(["src/main.py"])
    );
}

#[test]
fn runtime_boundary_reports_fixture_validation_errors_and_pairs_lifecycle() {
    let fixture = contract();
    let mut invalid_system_prompt = SubAgentConfig::new("parent-model", "Research");
    invalid_system_prompt.system_prompt = Some(" \n ".to_string());
    let cases = [
        (
            SubAgentConfig::new(" ", "Research"),
            fixture["validation"]["empty_model_error_code"]
                .as_str()
                .expect("model error code"),
            fixture["validation"]["empty_model_message"]
                .as_str()
                .expect("model error message"),
        ),
        (
            invalid_system_prompt,
            fixture["validation"]["empty_system_prompt_error_code"]
                .as_str()
                .expect("prompt error code"),
            fixture["validation"]["empty_system_prompt_message"]
                .as_str()
                .expect("prompt error message"),
        ),
    ];

    for (sub_agent, expected_code, expected_message) in cases {
        let llm = ScriptedLlmClient::new(vec![
            LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "delegate",
                    "create_sub_task",
                    json!({"agent_id": "researcher", "task_description": "Research"}),
                )],
            ),
            LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "parent-finish",
                    "task_finish",
                    json!({"message": "parent done"}),
                )],
            ),
        ]);
        let runtime = AgentRuntime::new(llm);
        let mut parent = AgentTask::new("parent-task", "parent-model", "Parent prompt", "Delegate");
        parent.max_cycles = 3;
        parent
            .sub_agents
            .insert("researcher".to_string(), sub_agent);
        let lifecycle = Arc::new(Mutex::new(Vec::new()));
        let lifecycle_for_handler = lifecycle.clone();
        let log_handler: vv_agent::RuntimeEventHandler = Arc::new(move |name, payload| {
            if matches!(name, "sub_run_started" | "sub_run_completed") {
                lifecycle_for_handler
                    .lock()
                    .expect("lifecycle")
                    .push((name.to_string(), payload.clone()));
            }
        });
        let controls = RuntimeRunControls {
            log_handler: Some(log_handler),
            execution_context: Some(ExecutionContext {
                metadata: BTreeMap::from([
                    ("_vv_agent_run_id".to_string(), json!("parent-run")),
                    ("_vv_agent_trace_id".to_string(), json!("trace-parity")),
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

        let result = runtime
            .run_with_controls(parent, controls)
            .expect("parent run");
        let tool_result = &result.cycles[0].tool_results[0];
        let payload: Value = serde_json::from_str(&tool_result.content).expect("error payload");
        assert_eq!(tool_result.error_code.as_deref(), Some(expected_code));
        assert_eq!(payload["error_code"], expected_code);
        assert_eq!(payload["error"], expected_message);
        let lifecycle = lifecycle.lock().expect("lifecycle");
        assert_eq!(
            lifecycle
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["sub_run_started", "sub_run_completed"]
        );
        assert_eq!(lifecycle[1].1["status"], "failed");
    }
}
