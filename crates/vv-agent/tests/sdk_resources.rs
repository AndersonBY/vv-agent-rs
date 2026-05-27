use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::{
    AgentDefinition, AgentResourceLoader, AgentRuntime, AgentSDKClient, AgentSDKOptions,
    AgentStatus, LLMResponse, LlmBuilder, LlmClient, NoToolPolicy, ResolvedModelConfig,
    ScriptedLlmClient,
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
