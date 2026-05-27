use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::{
    AfterLlmEvent, AgentDefinition, AgentResourceLoader, AgentRuntime, AgentSDKClient,
    AgentSDKOptions, AgentStatus, LLMResponse, LlmBuilder, LlmClient, LlmError, LlmRequest,
    Message, MessageRole, NoToolPolicy, ResolvedModelConfig, RuntimeHook, ScriptedLlmClient,
    ToolCall,
};

#[test]
fn resource_loader_discovers_agents_prompts_and_skills() {
    let workspace = tempfile::tempdir().expect("workspace");
    let resource_root = workspace.path().join(".vv-agent");
    std::fs::create_dir_all(resource_root.join("prompts")).expect("prompts");
    std::fs::create_dir_all(resource_root.join("skills/demo")).expect("skills");
    std::fs::write(
        resource_root.join("agents.json"),
        json!({
            "profiles": {
                "researcher": {
                    "description": "research profile",
                    "model": "kimi-k2.5",
                    "backend": "moonshot",
                    "language": "en-US",
                    "max_cycles": 12,
                    "memory_compact_threshold": 64000,
                    "memory_threshold_percentage": 80,
                    "no_tool_policy": "finish",
                    "allow_interruption": false,
                    "use_workspace": false,
                    "enable_todo_management": false,
                    "agent_type": "computer",
                    "native_multimodal": true,
                    "enable_sub_agents": true,
                    "sub_agents": {
                        "writer": {
                            "description": "write profile",
                            "model": "deepseek-v4-pro",
                            "backend": "deepseek",
                            "system_prompt": "write carefully",
                            "max_cycles": 5,
                            "exclude_tools": ["bash"],
                            "metadata": {"tier": "child"}
                        }
                    },
                    "extra_tool_names": ["custom_tool"],
                    "exclude_tools": ["read_image"],
                    "bash_shell": "bash",
                    "windows_shell_priority": ["powershell", "cmd"],
                    "bash_env": {"VV_AGENT_RESOURCE_ENV": "resource"},
                    "metadata": {"tier": "main"},
                    "system_prompt": "system",
                    "system_prompt_template": "research",
                    "skill_directories": ["skills"]
                }
            }
        })
        .to_string(),
    )
    .expect("agents");
    std::fs::write(
        resource_root.join("prompts/research.md"),
        "You are loaded from template.",
    )
    .expect("prompt");
    std::fs::write(
        resource_root.join("skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: demo skill\n---\nbody",
    )
    .expect("skill");

    let mut loader = AgentResourceLoader::with_resource_dirs(
        workspace.path(),
        &resource_root,
        workspace.path().join(".none"),
    );
    let discovered = loader.discover();

    assert!(discovered.agents.contains_key("researcher"));
    assert_eq!(
        discovered.agents["researcher"]
            .bash_env
            .get("VV_AGENT_RESOURCE_ENV")
            .map(String::as_str),
        Some("resource")
    );
    let researcher = &discovered.agents["researcher"];
    assert_eq!(researcher.language, "en-US");
    assert_eq!(researcher.max_cycles, 12);
    assert_eq!(researcher.memory_compact_threshold, 64000);
    assert_eq!(researcher.memory_threshold_percentage, 80);
    assert_eq!(researcher.no_tool_policy, vv_agent::NoToolPolicy::Finish);
    assert!(!researcher.allow_interruption);
    assert!(!researcher.use_workspace);
    assert!(!researcher.enable_todo_management);
    assert_eq!(researcher.agent_type.as_deref(), Some("computer"));
    assert!(researcher.native_multimodal);
    assert!(researcher.enable_sub_agents);
    assert_eq!(researcher.extra_tool_names, vec!["custom_tool"]);
    assert_eq!(researcher.exclude_tools, vec!["read_image"]);
    assert_eq!(researcher.bash_shell.as_deref(), Some("bash"));
    assert_eq!(researcher.windows_shell_priority, vec!["powershell", "cmd"]);
    assert_eq!(researcher.metadata["tier"], "main");
    assert_eq!(researcher.system_prompt.as_deref(), Some("system"));
    assert!(researcher
        .skill_directories
        .iter()
        .any(|path| path.ends_with(".vv-agent/skills")));
    let writer = &researcher.sub_agents["writer"];
    assert_eq!(writer.description, "write profile");
    assert_eq!(writer.model, "deepseek-v4-pro");
    assert_eq!(writer.backend.as_deref(), Some("deepseek"));
    assert_eq!(writer.system_prompt.as_deref(), Some("write carefully"));
    assert_eq!(writer.max_cycles, 5);
    assert_eq!(writer.exclude_tools, vec!["bash"]);
    assert_eq!(writer.metadata["tier"], "child");
    assert_eq!(
        discovered.prompts.get("research").map(String::as_str),
        Some("You are loaded from template.")
    );
    assert!(discovered
        .skill_directories
        .iter()
        .any(|path| path.ends_with("skills")));
    assert!(discovered.diagnostics.is_empty());
}

#[test]
fn resource_loader_force_reload_refreshes_cached_resources_like_python() {
    let workspace = tempfile::tempdir().expect("workspace");
    let resource_root = workspace.path().join(".vv-agent");
    std::fs::create_dir_all(resource_root.join("prompts")).expect("prompts");
    std::fs::write(resource_root.join("prompts/research.md"), "first").expect("first prompt");

    let mut loader = AgentResourceLoader::with_resource_dirs(
        workspace.path(),
        &resource_root,
        workspace.path().join(".none"),
    );
    let first = loader.discover();
    assert_eq!(
        first.prompts.get("research").map(String::as_str),
        Some("first")
    );

    std::fs::write(resource_root.join("prompts/research.md"), "second").expect("second prompt");
    let cached = loader.discover();
    assert_eq!(
        cached.prompts.get("research").map(String::as_str),
        Some("first")
    );

    let reloaded = loader.discover_force_reload();
    assert_eq!(
        reloaded.prompts.get("research").map(String::as_str),
        Some("second")
    );
}

#[test]
fn resource_loader_expands_home_paths_like_python() {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let workspace = tempfile::tempdir().expect("workspace");
    let loader = AgentResourceLoader::with_resource_dirs(
        workspace.path(),
        workspace.path().join(".vv-agent"),
        "~/.vv-agent-test",
    );

    assert_eq!(
        loader.global_resource_dir,
        std::path::PathBuf::from(home).join(".vv-agent-test")
    );
}

#[test]
fn resource_loader_reports_invalid_agent_profiles_like_python() {
    let workspace = tempfile::tempdir().expect("workspace");
    let resource_root = workspace.path().join(".vv-agent");
    std::fs::create_dir_all(&resource_root).expect("resource root");
    std::fs::write(
        resource_root.join("agents.json"),
        json!({
            "profiles": {
                "": {"description": "blank name", "model": "demo"},
                "not_object": "invalid",
                "missing_description": {"model": "demo"},
                "missing_model": {"description": "demo"}
            }
        })
        .to_string(),
    )
    .expect("agents");

    let mut loader = AgentResourceLoader::with_resource_dirs(
        workspace.path(),
        &resource_root,
        workspace.path().join(".none"),
    );
    let discovered = loader.discover();

    assert!(discovered.agents.is_empty());
    assert!(discovered
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.contains("Skip invalid profile name")));
    assert!(discovered
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.contains("definition must be an object")));
    assert!(discovered
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.contains("`description` must be non-empty string")));
    assert!(discovered
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.contains("`model` must be non-empty string")));
}

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
fn sdk_client_prepare_task_for_agent_uses_resources_like_python() {
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
fn sdk_client_run_requires_agent_when_no_profile_is_configured_like_python() {
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
fn sdk_client_run_requires_agent_when_multiple_profiles_are_configured_like_python() {
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
        settings_file: "settings.py".into(),
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
            "settings.py".to_string(),
            "custom-backend".to_string(),
            "demo-model".to_string(),
            12.5,
        )]
    );
}

#[test]
fn sdk_options_runtime_hooks_patch_llm_response_like_python() {
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
fn sdk_options_runtime_hooks_apply_to_injected_runtime_like_python() {
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
fn sdk_client_builds_python_style_system_prompt_from_agent_definition() {
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
