use super::*;

#[test]
fn sdk_runtime_uses_options_workspace_for_tool_context_and_sessions() {
    let workspace = tempfile::tempdir().expect("workspace");
    let responses = vec![LLMResponse {
        content: "finish".to_string(),
        tool_calls: vec![ToolCall::new(
            "finish-1",
            "task_finish",
            json_args(serde_json::json!({"message": "ok"})),
        )],
        raw: BTreeMap::new(),
        token_usage: TokenUsage::default(),
    }];
    let workspaces = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = AgentRuntime::new(ScriptedLlmClient::new(responses));
    runtime.hooks.push(Arc::new(WorkspaceRecordingHook {
        workspaces: Arc::clone(&workspaces),
    }));
    let client = AgentSDKClient::new(AgentSDKOptions {
        workspace: workspace.path().to_path_buf(),
        ..AgentSDKOptions::default()
    })
    .with_runtime(runtime);
    let session = create_agent_session(&client, "demo", AgentDefinition::default_for_model("demo"));

    let run = client
        .run_with_agent(AgentDefinition::default_for_model("demo"), "finish")
        .expect("run");

    assert_eq!(run.result.status, AgentStatus::Completed);
    assert_eq!(session.state().workspace, workspace.path());
    assert_eq!(
        workspaces.lock().expect("workspaces").as_slice(),
        &[workspace.path().to_path_buf()]
    );
}

#[test]
fn sdk_session_workspace_override_is_used_for_tool_context_and_state() {
    let default_workspace = tempfile::tempdir().expect("default workspace");
    let override_workspace = tempfile::tempdir().expect("override workspace");
    let responses = vec![
        LLMResponse {
            content: "write marker".to_string(),
            tool_calls: vec![ToolCall::new(
                "write-1",
                "write_file",
                json_args(serde_json::json!({
                    "path": "marker.txt",
                    "content": "from override"
                })),
            )],
            raw: BTreeMap::new(),
            token_usage: TokenUsage::default(),
        },
        LLMResponse {
            content: "finish".to_string(),
            tool_calls: vec![ToolCall::new(
                "finish-1",
                "task_finish",
                json_args(serde_json::json!({"message": "ok"})),
            )],
            raw: BTreeMap::new(),
            token_usage: TokenUsage::default(),
        },
    ];
    let client = AgentSDKClient::new(AgentSDKOptions {
        workspace: default_workspace.path().to_path_buf(),
        ..AgentSDKOptions::default()
    })
    .with_runtime(AgentRuntime::new(ScriptedLlmClient::new(responses)));
    let mut session = client.create_session_with_workspace(
        "demo",
        AgentDefinition::default_for_model("demo"),
        override_workspace.path(),
    );

    let run = session.prompt("write marker").expect("prompt");

    assert_eq!(run.result.status, AgentStatus::Completed);
    assert_eq!(session.state().workspace, override_workspace.path());
    assert_eq!(
        std::fs::read_to_string(override_workspace.path().join("marker.txt")).expect("marker"),
        "from override"
    );
    assert!(!default_workspace.path().join("marker.txt").exists());
}

#[test]
fn sdk_client_create_default_session_selects_only_profile_with_workspace() {
    let default_workspace = tempfile::tempdir().expect("default workspace");
    let override_workspace = tempfile::tempdir().expect("override workspace");
    let responses = vec![
        LLMResponse {
            content: "write marker".to_string(),
            tool_calls: vec![ToolCall::new(
                "write-1",
                "write_file",
                json_args(serde_json::json!({
                    "path": "marker.txt",
                    "content": "from default session"
                })),
            )],
            raw: BTreeMap::new(),
            token_usage: TokenUsage::default(),
        },
        LLMResponse {
            content: "finish".to_string(),
            tool_calls: vec![ToolCall::new(
                "finish-1",
                "task_finish",
                json_args(serde_json::json!({"message": "ok"})),
            )],
            raw: BTreeMap::new(),
            token_usage: TokenUsage::default(),
        },
    ];
    let mut client = AgentSDKClient::new(AgentSDKOptions {
        workspace: default_workspace.path().to_path_buf(),
        ..AgentSDKOptions::default()
    })
    .with_runtime(AgentRuntime::new(ScriptedLlmClient::new(responses)));
    client
        .register_agent("demo", AgentDefinition::default_for_model("demo"))
        .expect("register demo");

    let mut session = client
        .create_default_session_with_workspace(override_workspace.path())
        .expect("default session");
    let run = session.prompt("write marker").expect("prompt");

    assert_eq!(run.agent_name, "demo");
    assert_eq!(run.result.status, AgentStatus::Completed);
    assert_eq!(session.state().workspace, override_workspace.path());
    assert_eq!(
        std::fs::read_to_string(override_workspace.path().join("marker.txt")).expect("marker"),
        "from default session"
    );
    assert!(!default_workspace.path().join("marker.txt").exists());
}

#[test]
fn sdk_default_session_combines_id_workspace_and_shared_state() {
    let default_workspace = tempfile::tempdir().expect("default workspace");
    let override_workspace = tempfile::tempdir().expect("override workspace");
    let mut client = AgentSDKClient::new(AgentSDKOptions {
        workspace: default_workspace.path().to_path_buf(),
        ..AgentSDKOptions::default()
    });
    client
        .register_agent("demo", AgentDefinition::default_for_model("demo"))
        .expect("register demo");

    let session = client
        .create_default_session_with_id_workspace_and_shared_state(
            "session-fixed",
            override_workspace.path(),
            BTreeMap::from([(
                "seed".to_string(),
                Value::String("from-default-combo".to_string()),
            )]),
        )
        .expect("default session combo");

    assert_eq!(session.session_id(), "session-fixed");
    assert_eq!(session.agent_name(), "demo");
    assert_eq!(session.workspace(), override_workspace.path());
    assert_eq!(
        session.shared_state().get("seed").and_then(Value::as_str),
        Some("from-default-combo")
    );
    assert!(!default_workspace.path().join("marker.txt").exists());
}

#[test]
fn sdk_session_absolutizes_relative_workspace_override() {
    let current_dir = std::env::current_dir().expect("current dir");
    let root = tempfile::tempdir_in(&current_dir).expect("root");
    let default_workspace = root.path().join("default-workspace");
    let override_workspace = root.path().join("relative-workspace");
    std::fs::create_dir_all(&default_workspace).expect("default workspace");
    std::fs::create_dir_all(&override_workspace).expect("override workspace");
    let relative_workspace = std::path::PathBuf::from(
        root.path()
            .file_name()
            .expect("root file name")
            .to_string_lossy()
            .to_string(),
    )
    .join("relative-workspace");
    let client = AgentSDKClient::new(AgentSDKOptions {
        workspace: default_workspace,
        auto_discover_resources: false,
        ..AgentSDKOptions::default()
    });
    let session = client.create_session_with_workspace(
        "demo",
        AgentDefinition::default_for_model("demo"),
        &relative_workspace,
    );

    assert_eq!(session.workspace(), override_workspace.as_path());
}

#[test]
fn sdk_session_canonicalizes_workspace_override() {
    let root = tempfile::tempdir().expect("root");
    let default_workspace = root.path().join("default-workspace");
    let override_workspace = root.path().join("override-workspace");
    std::fs::create_dir_all(&default_workspace).expect("default workspace");
    std::fs::create_dir_all(&override_workspace).expect("override workspace");
    let noncanonical_workspace = root.path().join("nested/../override-workspace");
    std::fs::create_dir_all(root.path().join("nested")).expect("nested");
    let client = AgentSDKClient::new(AgentSDKOptions {
        workspace: default_workspace,
        auto_discover_resources: false,
        ..AgentSDKOptions::default()
    });

    let session = client.create_session_with_workspace(
        "demo",
        AgentDefinition::default_for_model("demo"),
        &noncanonical_workspace,
    );

    assert_eq!(session.workspace(), override_workspace.as_path());
    assert!(!session.workspace().to_string_lossy().contains(".."));
}

#[test]
fn sdk_named_session_combines_id_workspace_and_shared_state() {
    let default_workspace = tempfile::tempdir().expect("default workspace");
    let override_workspace = tempfile::tempdir().expect("override workspace");
    let mut client = AgentSDKClient::new(AgentSDKOptions {
        workspace: default_workspace.path().to_path_buf(),
        ..AgentSDKOptions::default()
    });
    client
        .register_agent("demo", AgentDefinition::default_for_model("demo"))
        .expect("register demo");

    let session = client
        .create_agent_session_by_name_with_id_workspace_and_shared_state(
            "demo",
            "named-session-fixed",
            override_workspace.path(),
            BTreeMap::from([(
                "seed".to_string(),
                Value::String("from-named-combo".to_string()),
            )]),
        )
        .expect("named session combo");

    assert_eq!(session.session_id(), "named-session-fixed");
    assert_eq!(session.agent_name(), "demo");
    assert_eq!(session.workspace(), override_workspace.path());
    assert_eq!(
        session.shared_state().get("seed").and_then(Value::as_str),
        Some("from-named-combo")
    );
}

#[test]
fn sdk_session_reuses_sub_task_manager_across_turns() {
    let llm = SessionSubTaskManagerLlm;
    let client = AgentSDKClient::new(AgentSDKOptions::default())
        .with_runtime(AgentRuntime::new(llm.clone()));
    let mut definition = AgentDefinition::default_for_model("demo");
    definition.enable_sub_agents = true;
    definition.sub_agents.insert(
        "researcher".to_string(),
        SubAgentConfig::new("demo", "research profile"),
    );
    let mut session = create_agent_session(&client, "demo", definition);

    let first = session.prompt("start child task").expect("first prompt");
    let second = session.prompt("check prior child").expect("second prompt");

    assert_eq!(first.result.final_answer.as_deref(), Some("created child"));
    assert_eq!(
        second.result.final_answer.as_deref(),
        Some("found prior child")
    );
}

#[test]
fn sdk_runtime_applies_startup_shell_defaults_to_tool_context() {
    let responses = vec![
        LLMResponse {
            content: "run shell".to_string(),
            tool_calls: vec![ToolCall::new(
                "bash-1",
                "bash",
                json_args(serde_json::json!({"command": "echo skipped"})),
            )],
            raw: BTreeMap::new(),
            token_usage: TokenUsage::default(),
        },
        LLMResponse {
            content: "finish".to_string(),
            tool_calls: vec![ToolCall::new(
                "finish-1",
                "task_finish",
                json_args(serde_json::json!({"message": "ok"})),
            )],
            raw: BTreeMap::new(),
            token_usage: TokenUsage::default(),
        },
    ];
    let captured_metadata = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = AgentRuntime::new(ScriptedLlmClient::new(responses));
    runtime.hooks.push(Arc::new(ShellMetadataCaptureHook {
        captured_metadata: Arc::clone(&captured_metadata),
    }));
    let mut client = AgentSDKClient::new(AgentSDKOptions {
        bash_shell: Some("powershell".to_string()),
        windows_shell_priority: vec!["git-bash".to_string(), "powershell".to_string()],
        bash_env: BTreeMap::from([
            (
                "VV_AGENT_OPTION_ONLY".to_string(),
                "from-option".to_string(),
            ),
            ("VV_AGENT_SHARED".to_string(), "from-option".to_string()),
        ]),
        ..AgentSDKOptions::default()
    })
    .with_runtime(runtime);
    let mut definition = AgentDefinition::default_for_model("demo");
    definition.extra_tool_names = vec!["bash".to_string()];
    definition.bash_env = BTreeMap::from([
        ("VV_AGENT_AGENT_ONLY".to_string(), "from-agent".to_string()),
        ("VV_AGENT_SHARED".to_string(), "from-agent".to_string()),
    ]);
    client.set_default_agent(definition);

    let run = client.query("run shell").expect("query");

    assert_eq!(run, "ok");
    let captured = captured_metadata.lock().expect("captured metadata");
    let metadata = captured.first().expect("bash metadata");
    assert_eq!(metadata["bash_shell"], "powershell");
    assert_eq!(
        metadata["windows_shell_priority"],
        serde_json::json!(["git-bash", "powershell"])
    );
    assert_eq!(metadata["bash_env"]["VV_AGENT_OPTION_ONLY"], "from-option");
    assert_eq!(metadata["bash_env"]["VV_AGENT_AGENT_ONLY"], "from-agent");
    assert_eq!(metadata["bash_env"]["VV_AGENT_SHARED"], "from-agent");
}

#[test]
fn sdk_session_applies_startup_shell_defaults_to_tool_context() {
    let responses = vec![
        LLMResponse {
            content: "run shell".to_string(),
            tool_calls: vec![ToolCall::new(
                "bash-1",
                "bash",
                json_args(serde_json::json!({"command": "echo skipped"})),
            )],
            raw: BTreeMap::new(),
            token_usage: TokenUsage::default(),
        },
        LLMResponse {
            content: "finish".to_string(),
            tool_calls: vec![ToolCall::new(
                "finish-1",
                "task_finish",
                json_args(serde_json::json!({"message": "ok"})),
            )],
            raw: BTreeMap::new(),
            token_usage: TokenUsage::default(),
        },
    ];
    let captured_metadata = Arc::new(Mutex::new(Vec::new()));
    let mut runtime = AgentRuntime::new(ScriptedLlmClient::new(responses));
    runtime.hooks.push(Arc::new(ShellMetadataCaptureHook {
        captured_metadata: Arc::clone(&captured_metadata),
    }));
    let client = AgentSDKClient::new(AgentSDKOptions {
        bash_shell: Some("powershell".to_string()),
        windows_shell_priority: vec!["git-bash".to_string(), "powershell".to_string()],
        bash_env: BTreeMap::from([
            (
                "VV_AGENT_OPTION_ONLY".to_string(),
                "from-option".to_string(),
            ),
            ("VV_AGENT_SHARED".to_string(), "from-option".to_string()),
        ]),
        ..AgentSDKOptions::default()
    })
    .with_runtime(runtime);
    let mut definition = AgentDefinition::default_for_model("demo");
    definition.extra_tool_names = vec!["bash".to_string()];
    definition.bash_env = BTreeMap::from([
        ("VV_AGENT_AGENT_ONLY".to_string(), "from-agent".to_string()),
        ("VV_AGENT_SHARED".to_string(), "from-agent".to_string()),
    ]);
    let mut session = create_agent_session(&client, "demo", definition);

    let run = session.prompt("run shell").expect("session prompt");

    assert_eq!(run.result.final_answer.as_deref(), Some("ok"));
    let captured = captured_metadata.lock().expect("captured metadata");
    let metadata = captured.first().expect("bash metadata");
    assert_eq!(metadata["bash_shell"], "powershell");
    assert_eq!(
        metadata["windows_shell_priority"],
        serde_json::json!(["git-bash", "powershell"])
    );
    assert_eq!(metadata["bash_env"]["VV_AGENT_OPTION_ONLY"], "from-option");
    assert_eq!(metadata["bash_env"]["VV_AGENT_AGENT_ONLY"], "from-agent");
    assert_eq!(metadata["bash_env"]["VV_AGENT_SHARED"], "from-agent");
}
