use super::*;

#[test]
fn sdk_module_level_run_with_options_and_agent_helper() {
    let workspace = tempfile::tempdir().expect("workspace");
    let builder: LlmBuilder = Arc::new(move |_settings_path, backend, model, _timeout_seconds| {
        let llm: Arc<dyn LlmClient> =
            Arc::new(ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
                "done",
                vec![ToolCall::new(
                    "finish",
                    "task_finish",
                    BTreeMap::from([("message".to_string(), json!("module-run"))]),
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
    let options = AgentSDKOptions {
        auto_discover_resources: false,
        workspace: workspace.path().to_path_buf(),
        llm_builder: Some(builder),
        tool_registry_factory: Some(Arc::new(build_default_registry)),
        ..AgentSDKOptions::default()
    };

    let run = vv_agent::run_with_options_and_agent(
        options,
        AgentDefinition::default_for_model("demo-model"),
        "say ok",
    )
    .expect("module run helper");

    assert_eq!(run.result.final_answer.as_deref(), Some("module-run"));
}

#[test]
fn sdk_module_level_query_with_options_and_agent_helper() {
    let workspace = tempfile::tempdir().expect("workspace");
    let builder: LlmBuilder = Arc::new(move |_settings_path, backend, model, _timeout_seconds| {
        let llm: Arc<dyn LlmClient> =
            Arc::new(ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
                "done",
                vec![ToolCall::new(
                    "finish",
                    "task_finish",
                    BTreeMap::from([("message".to_string(), json!("module-query"))]),
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
    let options = AgentSDKOptions {
        auto_discover_resources: false,
        workspace: workspace.path().to_path_buf(),
        llm_builder: Some(builder),
        tool_registry_factory: Some(Arc::new(build_default_registry)),
        ..AgentSDKOptions::default()
    };

    let text = vv_agent::query_with_options_and_agent(
        options,
        AgentDefinition::default_for_model("demo-model"),
        "say ok",
    )
    .expect("module query helper");

    assert_eq!(text, "module-query");
}

#[test]
fn sdk_options_tool_registry_factory_runs_custom_tools() {
    let custom_tool = "_workflow_custom_run";
    let builder: LlmBuilder = Arc::new(move |_, backend, model, _| {
        let llm: Arc<dyn LlmClient> = Arc::new(ScriptedLlmClient::new(vec![
            LLMResponse::with_tool_calls(
                "run custom workflow",
                vec![ToolCall::new(
                    "custom_call",
                    custom_tool,
                    BTreeMap::from([("workflow".to_string(), json!("wf_translate"))]),
                )],
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
            .register_tool_with_parameters(
                custom_tool,
                "Run workflow via custom integration layer.",
                json!({
                    "type": "object",
                    "properties": {"workflow": {"type": "string"}},
                    "required": ["workflow"],
                }),
                Arc::new(|context, arguments| {
                    if !matches!(
                        context.execution_backend.as_ref(),
                        Some(RuntimeExecutionBackend::Thread(_))
                    ) {
                        let mut result =
                            ToolExecutionResult::error("", "missing SDK execution backend");
                        result.error_code = Some("missing_execution_backend".to_string());
                        return result;
                    }
                    context.shared_state.insert(
                        "custom_workflow".to_string(),
                        arguments.get("workflow").cloned().unwrap_or(json!(null)),
                    );
                    ToolExecutionResult::success("", json!({"ok": true}).to_string())
                }),
            )
            .expect("register custom tool");
        registry
    });
    let client = AgentSDKClient::new(AgentSDKOptions {
        auto_discover_resources: false,
        llm_builder: Some(builder),
        tool_registry_factory: Some(factory),
        execution_backend: Some(RuntimeExecutionBackend::Thread(ThreadBackend::new(2))),
        ..AgentSDKOptions::default()
    });
    let mut agent = AgentDefinition::default_for_model("demo-model");
    agent.extra_tool_names.push(custom_tool.to_string());

    let run = client
        .run_with_agent(agent, "run the custom workflow")
        .expect("run through custom tool registry");

    assert_eq!(run.result.status, AgentStatus::Completed);
    assert_eq!(run.result.final_answer.as_deref(), Some("done"));
    assert_eq!(
        run.result.shared_state["custom_workflow"],
        json!("wf_translate")
    );
    assert_eq!(
        run.result.cycles[0].tool_results[0].status,
        ToolResultStatus::Success
    );
}
