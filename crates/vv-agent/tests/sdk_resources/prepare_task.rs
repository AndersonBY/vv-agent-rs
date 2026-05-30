use super::*;

#[test]
fn sdk_prepare_task_resolves_relative_skill_directories_from_workspace() {
    let workspace = tempfile::tempdir().expect("workspace");
    let skill_dir = workspace.path().join("skills/alpha");
    std::fs::create_dir_all(&skill_dir).expect("skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: alpha\ndescription: Alpha skill\n---\nBody\n",
    )
    .expect("skill");
    let client = AgentSDKClient::new(AgentSDKOptions {
        workspace: workspace.path().to_path_buf(),
        auto_discover_resources: false,
        ..AgentSDKOptions::default()
    });
    let mut agent = AgentDefinition::default_for_model("demo-model");
    agent.description = "assist".to_string();
    agent.skill_directories = vec!["skills".to_string()];

    let task = client.prepare_task_with_agent(agent, "hello", "demo-model");

    assert!(task.system_prompt.contains("<available_skills>"));
    assert!(task.system_prompt.contains("<name>\nalpha\n</name>"));
    assert_eq!(task.metadata["available_skills"], json!(["skills"]));
}

#[test]
fn sdk_prepare_task_clamps_runtime_limits() {
    let workspace = tempfile::tempdir().expect("workspace");
    let client = AgentSDKClient::new(AgentSDKOptions {
        workspace: workspace.path().to_path_buf(),
        auto_discover_resources: false,
        ..AgentSDKOptions::default()
    });
    let mut agent = AgentDefinition::default_for_model("demo-model");
    agent.max_cycles = 0;
    agent.memory_compact_threshold = 0;
    agent.memory_threshold_percentage = 130;

    let task = client.prepare_task_with_agent(agent, "hello", "demo-model");

    assert_eq!(task.max_cycles, 1);
    assert_eq!(task.memory_compact_threshold, 1);
    assert_eq!(task.memory_threshold_percentage, 100);
}

#[test]
fn sdk_prepare_task_accepts_explicit_session_id() {
    let workspace = tempfile::tempdir().expect("workspace");
    let client = AgentSDKClient::new(AgentSDKOptions {
        workspace: workspace.path().to_path_buf(),
        auto_discover_resources: false,
        ..AgentSDKOptions::default()
    });
    let mut agent = AgentDefinition::default_for_model("demo-model");
    agent
        .metadata
        .insert("session_id".to_string(), json!("definition-session"));

    let inline_task = client.prepare_task_with_agent_with_session_id(
        agent.clone(),
        "hello",
        "demo-model",
        "session-preview",
    );
    assert_eq!(
        inline_task.metadata["session_id"],
        json!("definition-session")
    );

    let mut client = AgentSDKClient::new_with_agent(
        AgentSDKOptions {
            workspace: workspace.path().to_path_buf(),
            auto_discover_resources: false,
            ..AgentSDKOptions::default()
        },
        AgentDefinition::default_for_model("demo-model"),
    );
    client
        .register_agent(
            "researcher",
            AgentDefinition::default_for_model("demo-model"),
        )
        .expect("register agent");

    let named_task = client
        .prepare_task_for_agent_with_session_id(
            "researcher",
            "preview named",
            "demo-model",
            "session-named",
        )
        .expect("named task");
    assert_eq!(named_task.metadata["session_id"], json!("session-named"));

    let default_task = client
        .prepare_task_with_session_id("preview default", "demo-model", "session-default")
        .expect("default task");
    assert_eq!(
        default_task.metadata["session_id"],
        json!("session-default")
    );
}

#[test]
fn sdk_prepare_task_with_request_accepts_task_name_workspace_and_metadata() {
    let workspace = tempfile::tempdir().expect("workspace");
    let override_workspace = tempfile::tempdir().expect("override workspace");
    let client = AgentSDKClient::new_with_agent(
        AgentSDKOptions {
            workspace: workspace.path().to_path_buf(),
            auto_discover_resources: false,
            ..AgentSDKOptions::default()
        },
        {
            let mut agent = AgentDefinition::default_for_model("demo-model");
            agent.skill_directories = vec!["skills/preview".to_string()];
            agent
        },
    );
    std::fs::create_dir_all(override_workspace.path().join("skills/preview")).expect("skill dir");
    std::fs::write(
        override_workspace.path().join("skills/preview/SKILL.md"),
        "---\nname: preview-skill\ndescription: preview skill\n---\nbody",
    )
    .expect("skill file");

    let mut request = AgentSessionRunRequest::new("preview request");
    request.task_name = Some("custom-preview".to_string());
    request.workspace = Some(override_workspace.path().to_path_buf());
    request
        .metadata
        .insert("session_id".to_string(), json!("session-request"));
    request
        .metadata
        .insert("model_context_window".to_string(), json!(1234));
    request
        .shared_state
        .insert("should_not_leak".to_string(), json!(true));
    request
        .initial_messages
        .push(Message::user("previous turn"));

    let task = client
        .prepare_task_with_request(request, "demo-model")
        .expect("prepared task");

    assert!(task.task_id.starts_with("custom-preview_"));
    assert_eq!(task.user_prompt, "preview request");
    assert_eq!(task.metadata["session_id"], json!("session-request"));
    assert_eq!(task.metadata["model_context_window"], json!(1234));
    assert!(
        !task.metadata.contains_key("should_not_leak"),
        "request shared_state should only be used by runtime execution, not task preparation"
    );
    assert_eq!(task.initial_messages.len(), 0);
    assert!(task.system_prompt.contains("preview-skill"));
    assert!(task.system_prompt.contains("preview skill"));
}

#[test]
fn sdk_client_run_requires_agent_when_no_profile_is_configured() {
    let workspace = tempfile::tempdir().expect("workspace");
    let runtime = AgentRuntime::new(ScriptedLlmClient::new(vec![LLMResponse::new(
        "should not run",
    )]));
    let client = AgentSDKClient::new(AgentSDKOptions {
        workspace: workspace.path().to_path_buf(),
        auto_discover_resources: false,
        ..AgentSDKOptions::default()
    })
    .with_runtime(runtime);

    let error = client
        .run("use no configured profile")
        .expect_err("no profile");

    assert!(error.contains("No agent configured"));
}

#[test]
fn sdk_client_query_agent_helper_returns_text() {
    let workspace = tempfile::tempdir().expect("workspace");
    let runtime = AgentRuntime::new(ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "done",
        vec![ToolCall::new(
            "finish",
            "task_finish",
            BTreeMap::from([("message".to_string(), json!("query-helper"))]),
        )],
    )]));
    let mut client = AgentSDKClient::new(AgentSDKOptions {
        workspace: workspace.path().to_path_buf(),
        auto_discover_resources: false,
        ..AgentSDKOptions::default()
    })
    .with_runtime(runtime);
    client
        .register_agent("demo", AgentDefinition::default_for_model("demo-model"))
        .expect("register demo");

    let text = client.query_agent("demo", "say ok").expect("query agent");

    assert_eq!(text, "query-helper");
}

#[test]
fn sdk_client_query_agent_can_return_wait_reason_when_not_strict() {
    let workspace = tempfile::tempdir().expect("workspace");
    let runtime = AgentRuntime::new(ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "need input",
        vec![ToolCall::new(
            "ask",
            "ask_user",
            BTreeMap::from([("question".to_string(), json!("pick one"))]),
        )],
    )]));
    let mut client = AgentSDKClient::new(AgentSDKOptions {
        workspace: workspace.path().to_path_buf(),
        auto_discover_resources: false,
        ..AgentSDKOptions::default()
    })
    .with_runtime(runtime);
    client
        .register_agent("demo", AgentDefinition::default_for_model("demo-model"))
        .expect("register demo");

    let text = client
        .query_agent_with_require_completed("demo", "say ok", false)
        .expect("query wait reason");

    assert!(text.contains("pick one"));
}

#[test]
fn sdk_client_run_requires_agent_when_multiple_profiles_are_configured() {
    let workspace = tempfile::tempdir().expect("workspace");
    let runtime = AgentRuntime::new(ScriptedLlmClient::new(vec![LLMResponse::new(
        "should not run",
    )]));
    let mut client = AgentSDKClient::new(AgentSDKOptions {
        workspace: workspace.path().to_path_buf(),
        auto_discover_resources: false,
        ..AgentSDKOptions::default()
    })
    .with_runtime(runtime);
    client
        .register_agent("a", AgentDefinition::default_for_model("model-a"))
        .expect("register a");
    client
        .register_agent("b", AgentDefinition::default_for_model("model-b"))
        .expect("register b");

    let error = client
        .run("ambiguous profile")
        .expect_err("multiple profiles");

    assert!(error.contains("Multiple agents configured"));
    assert!(error.contains("a"));
    assert!(error.contains("b"));
}

#[test]
fn sdk_client_uses_llm_builder_when_runtime_is_not_injected() {
    let calls = Arc::new(Mutex::new(Vec::<(String, String, String, f64)>::new()));
    let builder: LlmBuilder = {
        let calls = Arc::clone(&calls);
        Arc::new(move |settings_path, backend, model, timeout_seconds| {
            calls.lock().expect("calls").push((
                settings_path.display().to_string(),
                backend.to_string(),
                model.to_string(),
                timeout_seconds,
            ));
            let llm: Arc<dyn LlmClient> = Arc::new(ScriptedLlmClient::new(vec![LLMResponse::new(
                "builder answer",
            )]));
            Ok((
                llm,
                ResolvedModelConfig::new(
                    backend.to_string(),
                    model.to_string(),
                    model.to_string(),
                    format!("{model}-resolved"),
                    Vec::new(),
                ),
            ))
        })
    };
    let client = AgentSDKClient::new(AgentSDKOptions {
        settings_file: "settings.json".into(),
        default_backend: "deepseek".to_string(),
        timeout_seconds: 12.5,
        auto_discover_resources: false,
        llm_builder: Some(builder),
        ..AgentSDKOptions::default()
    });
    let mut agent = AgentDefinition::default_for_model("demo-model");
    agent.backend = Some("custom-backend".to_string());
    agent.no_tool_policy = NoToolPolicy::Finish;

    let run = client
        .run_with_agent(agent, "use configured builder")
        .expect("run through builder");

    assert_eq!(run.result.status, AgentStatus::Completed);
    assert_eq!(run.result.final_answer.as_deref(), Some("builder answer"));
    assert_eq!(run.resolved.backend, "custom-backend");
    assert_eq!(run.resolved.model_id, "demo-model-resolved");
    assert_eq!(
        *calls.lock().expect("calls"),
        vec![(
            "settings.json".to_string(),
            "custom-backend".to_string(),
            "demo-model".to_string(),
            12.5,
        )]
    );
}

#[test]
fn sdk_one_shot_run_can_override_workspace() {
    let root = tempfile::tempdir().expect("root");
    let default_workspace = root.path().join("default-workspace");
    let override_workspace = root.path().join("override-workspace");
    let builder: LlmBuilder = Arc::new(move |_settings_path, backend, model, _timeout_seconds| {
        let llm: Arc<dyn LlmClient> = Arc::new(ScriptedLlmClient::new(vec![
            LLMResponse::with_tool_calls(
                "write marker",
                vec![ToolCall::new(
                    "write-marker",
                    "write_file",
                    BTreeMap::from([
                        ("path".to_string(), json!("marker.txt")),
                        ("content".to_string(), json!("workspace override")),
                    ]),
                )],
            ),
            LLMResponse::with_tool_calls(
                "finish",
                vec![ToolCall::new(
                    "finish",
                    "task_finish",
                    BTreeMap::from([("message".to_string(), json!("ok"))]),
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
    let client = AgentSDKClient::new(AgentSDKOptions {
        auto_discover_resources: false,
        workspace: default_workspace.clone(),
        llm_builder: Some(builder),
        tool_registry_factory: Some(Arc::new(build_default_registry)),
        ..AgentSDKOptions::default()
    });
    let mut agent = AgentDefinition::default_for_model("demo-model");
    agent.max_cycles = 3;

    let run = client
        .run_with_agent_in_workspace(agent, "write marker", &override_workspace)
        .expect("run with workspace override");

    assert_eq!(run.result.status, AgentStatus::Completed);
    assert_eq!(
        std::fs::read_to_string(override_workspace.join("marker.txt")).expect("marker"),
        "workspace override"
    );
    assert!(!default_workspace.join("marker.txt").exists());
}
