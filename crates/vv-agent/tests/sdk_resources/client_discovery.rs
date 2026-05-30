use super::*;

#[test]
fn sdk_client_auto_discovers_resource_agents_and_runs_by_name() {
    let workspace = tempfile::tempdir().expect("workspace");
    let resource_root = workspace.path().join(".vv-agent");
    std::fs::create_dir_all(&resource_root).expect("resource root");
    std::fs::write(
        resource_root.join("agents.json"),
        json!({
            "profiles": {
                "researcher": {
                    "description": "research profile",
                    "model": "demo-model",
                    "no_tool_policy": "finish"
                }
            }
        })
        .to_string(),
    )
    .expect("agents");

    let runtime = AgentRuntime::new(ScriptedLlmClient::new(vec![LLMResponse::new(
        "discovered answer",
    )]));
    let client = AgentSDKClient::new(AgentSDKOptions {
        workspace: workspace.path().to_path_buf(),
        ..AgentSDKOptions::default()
    })
    .with_runtime(runtime);

    assert_eq!(client.list_agents(), vec!["researcher".to_string()]);

    let run = client
        .run_agent("researcher", "use discovered profile")
        .expect("run discovered agent");

    assert_eq!(run.agent_name, "researcher");
    assert_eq!(run.result.status, AgentStatus::Completed);
    assert_eq!(
        run.result.final_answer.as_deref(),
        Some("discovered answer")
    );
}

#[test]
fn sdk_client_prepare_task_for_agent_uses_resources() {
    let workspace = tempfile::tempdir().expect("workspace");
    let resource_root = workspace.path().join(".vv-agent");
    std::fs::create_dir_all(resource_root.join("prompts")).expect("prompts");
    std::fs::create_dir_all(resource_root.join("skills/demo")).expect("skills");
    std::fs::write(
        resource_root.join("agents.json"),
        json!({
            "profiles": {
                "researcher": {
                    "description": "fallback",
                    "model": "demo-model",
                    "system_prompt_template": "research"
                }
            }
        })
        .to_string(),
    )
    .expect("agents");
    std::fs::write(
        resource_root.join("prompts/research.md"),
        "Template system prompt",
    )
    .expect("prompt");
    std::fs::write(
        resource_root.join("skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: demo skill\n---\nbody",
    )
    .expect("skill");

    let client = AgentSDKClient::new(AgentSDKOptions {
        workspace: workspace.path().to_path_buf(),
        ..AgentSDKOptions::default()
    });

    let task = client
        .prepare_task_for_agent("researcher", "preview task", "demo-model-resolved")
        .expect("prepare task");

    assert_eq!(task.model, "demo-model-resolved");
    assert_eq!(task.user_prompt, "preview task");
    assert!(task.system_prompt.contains("Template system prompt"));
    assert!(task.metadata["available_skills"]
        .as_array()
        .expect("skills")
        .iter()
        .any(|path| path.as_str().is_some_and(|path| path.ends_with("skills"))));
    assert_eq!(
        task.metadata["system_prompt_sections"][0]["id"],
        "agent_definition"
    );
}

#[test]
fn sdk_client_new_with_agent_prepares_default_task() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut agent = AgentDefinition::default_for_model("demo-model");
    agent.description = "default inline agent".to_string();

    let client = AgentSDKClient::new_with_agent(
        AgentSDKOptions {
            workspace: workspace.path().to_path_buf(),
            auto_discover_resources: false,
            ..AgentSDKOptions::default()
        },
        agent,
    );

    let task = client
        .prepare_task("preview default", "demo-model-resolved")
        .expect("prepare default task");

    assert_eq!(task.model, "demo-model-resolved");
    assert_eq!(task.user_prompt, "preview default");
    assert_agent_task_id(&task.task_id, "default");
    assert!(task.system_prompt.contains("default inline agent"));
}

#[test]
fn sdk_default_agent_definition_stays_capability_focused_in_prompt() {
    let workspace = tempfile::tempdir().expect("workspace");
    let client = AgentSDKClient::new_with_agent(
        AgentSDKOptions {
            workspace: workspace.path().to_path_buf(),
            auto_discover_resources: false,
            ..AgentSDKOptions::default()
        },
        AgentDefinition::default_for_model("demo-model"),
    );

    let task = client
        .prepare_task("preview default", "demo-model-resolved")
        .expect("prepare default task");

    assert!(task
        .system_prompt
        .contains("General-purpose VectorVein agent profile"));
    assert!(
        !task.system_prompt.to_ascii_lowercase().contains("rust"),
        "Agent-visible default prompt should not mention implementation language:\n{}",
        task.system_prompt
    );
}

#[test]
fn sdk_client_new_with_agents_resolves_only_agent() {
    let workspace = tempfile::tempdir().expect("workspace");
    let client = AgentSDKClient::new_with_agents(
        AgentSDKOptions {
            workspace: workspace.path().to_path_buf(),
            auto_discover_resources: false,
            ..AgentSDKOptions::default()
        },
        BTreeMap::from([(
            "researcher".to_string(),
            AgentDefinition::default_for_model("demo-model"),
        )]),
    )
    .expect("client with agents");

    assert_eq!(client.list_agents(), vec!["researcher"]);

    let task = client
        .prepare_task("preview only profile", "demo-model")
        .expect("prepare only agent");

    assert_agent_task_id(&task.task_id, "researcher");
    assert_eq!(task.user_prompt, "preview only profile");
}

#[test]
fn sdk_prepare_and_run_use_agent_unique_task_ids() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut client = AgentSDKClient::new(AgentSDKOptions {
        workspace: workspace.path().to_path_buf(),
        auto_discover_resources: false,
        ..AgentSDKOptions::default()
    });
    client
        .register_agent(
            "researcher",
            AgentDefinition::default_for_model("demo-model"),
        )
        .expect("register researcher");

    let first = client
        .prepare_task_for_agent("researcher", "preview one", "demo-model")
        .expect("first task");
    let second = client
        .prepare_task_for_agent("researcher", "preview two", "demo-model")
        .expect("second task");
    assert_agent_task_id(&first.task_id, "researcher");
    assert_agent_task_id(&second.task_id, "researcher");
    assert_ne!(first.task_id, second.task_id);

    let inline = client.prepare_task_with_agent(
        AgentDefinition::default_for_model("demo-model"),
        "preview inline",
        "demo-model",
    );
    assert_agent_task_id(&inline.task_id, "inline");

    let captured_task_ids = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut runtime =
        AgentRuntime::new(ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "finish_task_id",
                "task_finish",
                BTreeMap::from([("message".to_string(), json!("ok"))]),
            )],
        )]));
    runtime.hooks.push(Arc::new(TaskIdCaptureHook {
        captured_task_ids: Arc::clone(&captured_task_ids),
    }));
    let client = client.with_runtime(runtime);

    let run = client
        .run_agent("researcher", "execute task")
        .expect("run researcher");

    assert_eq!(run.result.status, AgentStatus::Completed);
    let captured = captured_task_ids.lock().expect("captured task ids");
    assert_eq!(captured.len(), 1);
    assert_agent_task_id(&captured[0], "researcher");
}

fn assert_agent_task_id(task_id: &str, prefix: &str) {
    let Some(suffix) = task_id.strip_prefix(&format!("{prefix}_")) else {
        panic!("task id {task_id:?} did not start with {prefix}_");
    };
    assert_eq!(suffix.len(), 8);
    assert!(
        suffix.chars().all(|ch| ch.is_ascii_hexdigit()),
        "task id suffix {suffix:?} should be 8 hex chars"
    );
}

struct TaskIdCaptureHook {
    captured_task_ids: Arc<Mutex<Vec<String>>>,
}

impl RuntimeHook for TaskIdCaptureHook {
    fn before_llm(&self, event: BeforeLlmEvent<'_>) -> Option<vv_agent::BeforeLlmPatch> {
        self.captured_task_ids
            .lock()
            .expect("captured task ids")
            .push(event.task.task_id.clone());
        None
    }
}
