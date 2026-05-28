use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::{
    build_default_registry, run_with_options_and_agent_request, AfterLlmEvent, AgentDefinition,
    AgentResourceLoader, AgentRuntime, AgentSDKClient, AgentSDKOptions, AgentSessionRunRequest,
    AgentStatus, BeforeLlmEvent, LLMResponse, LlmBuilder, LlmClient, LlmError, LlmRequest, Message,
    MessageRole, NoToolPolicy, ResolvedModelConfig, RuntimeExecutionBackend, RuntimeHook,
    ScriptedLlmClient, ThreadBackend, ToolCall, ToolDirective, ToolExecutionResult,
    ToolRegistryFactory, ToolResultStatus,
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
fn sdk_options_resource_loader_discovers_from_custom_roots_like_python() {
    let workspace = tempfile::tempdir().expect("workspace");
    let custom_root = workspace.path().join("custom-resources");
    std::fs::create_dir_all(custom_root.join("prompts")).expect("prompts");
    std::fs::write(
        custom_root.join("agents.json"),
        json!({
            "profiles": {
                "custom": {
                    "description": "custom profile",
                    "model": "deepseek-v4-pro",
                    "system_prompt_template": "custom-template"
                }
            }
        })
        .to_string(),
    )
    .expect("agents");
    std::fs::write(
        custom_root.join("prompts/custom-template.md"),
        "Loaded from injected resource loader.",
    )
    .expect("prompt");

    let client = AgentSDKClient::new(AgentSDKOptions {
        workspace: workspace.path().join("workspace-without-resources"),
        resource_loader: Some(AgentResourceLoader::with_resource_dirs(
            workspace.path(),
            &custom_root,
            workspace.path().join(".none"),
        )),
        ..AgentSDKOptions::default()
    });

    assert_eq!(client.list_agents(), vec!["custom"]);
    let task = client
        .prepare_task_for_agent("custom", "hello", "resolved-model")
        .expect("custom agent from injected loader");
    assert!(task
        .system_prompt
        .contains("Loaded from injected resource loader."));
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
fn resource_loader_canonicalizes_relative_skill_directories_like_python() {
    let workspace = tempfile::tempdir().expect("workspace");
    let resource_root = workspace.path().join(".vv-agent");
    let shared_skills = workspace.path().join("shared-skills");
    std::fs::create_dir_all(&resource_root).expect("resource root");
    std::fs::create_dir_all(&shared_skills).expect("shared skills");
    std::fs::write(
        resource_root.join("agents.json"),
        json!({
            "profiles": {
                "researcher": {
                    "description": "research profile",
                    "model": "demo-model",
                    "skill_directories": ["../shared-skills"]
                }
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

    assert_eq!(
        discovered.agents["researcher"].skill_directories,
        vec![shared_skills.to_string_lossy().to_string()]
    );
    assert!(!discovered.agents["researcher"].skill_directories[0].contains(".."));
}

#[test]
fn resource_loader_parses_agent_booleans_with_python_truthiness() {
    let workspace = tempfile::tempdir().expect("workspace");
    let resource_root = workspace.path().join(".vv-agent");
    std::fs::create_dir_all(&resource_root).expect("resource root");
    std::fs::write(
        resource_root.join("agents.json"),
        json!({
            "profiles": {
                "truthy": {
                    "description": "truthy profile",
                    "model": "demo-model",
                    "allow_interruption": 0,
                    "use_workspace": "",
                    "enable_todo_management": "false",
                    "native_multimodal": 0.0,
                    "enable_sub_agents": {"value": false}
                }
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
    let agent = &discovered.agents["truthy"];

    assert!(!agent.allow_interruption);
    assert!(!agent.use_workspace);
    assert!(agent.enable_todo_management);
    assert!(!agent.native_multimodal);
    assert!(agent.enable_sub_agents);
}

#[test]
fn resource_loader_stringifies_shell_lists_and_env_like_python() {
    let workspace = tempfile::tempdir().expect("workspace");
    let resource_root = workspace.path().join(".vv-agent");
    std::fs::create_dir_all(&resource_root).expect("resource root");
    std::fs::write(
        resource_root.join("agents.json"),
        json!({
            "profiles": {
                "shells": {
                    "description": "shell profile",
                    "model": "demo-model",
                    "windows_shell_priority": ["powershell", 7, true, null, ""],
                    "bash_env": {
                        "TEXT": "value",
                        "COUNT": 7,
                        "FLAG": true,
                        "NONE": null,
                        "EMPTY_KEY": "keep"
                    }
                }
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
    let agent = &discovered.agents["shells"];

    assert_eq!(
        agent.windows_shell_priority,
        vec!["powershell", "7", "True", "None"]
    );
    assert_eq!(
        agent.bash_env,
        BTreeMap::from([
            ("COUNT".to_string(), "7".to_string()),
            ("EMPTY_KEY".to_string(), "keep".to_string()),
            ("FLAG".to_string(), "True".to_string()),
            ("NONE".to_string(), "None".to_string()),
            ("TEXT".to_string(), "value".to_string()),
        ])
    );
}

#[test]
fn resource_loader_canonicalizes_root_skill_directory_like_python() {
    let workspace = tempfile::tempdir().expect("workspace");
    let resource_root = workspace.path().join(".vv-agent");
    let noncanonical_resource_root = workspace.path().join("nested/../.vv-agent");
    let root_skills = resource_root.join("skills");
    std::fs::create_dir_all(workspace.path().join("nested")).expect("nested");
    std::fs::create_dir_all(&root_skills).expect("skills");

    let mut loader = AgentResourceLoader::with_resource_dirs(
        workspace.path(),
        &noncanonical_resource_root,
        workspace.path().join(".none"),
    );
    let discovered = loader.discover();

    assert_eq!(
        discovered.skill_directories,
        vec![root_skills.to_string_lossy().to_string()]
    );
}

#[test]
fn resource_loader_canonicalizes_resource_roots_like_python() {
    let workspace = tempfile::tempdir().expect("workspace");
    let canonical_project_root = workspace.path().join(".vv-agent");
    let canonical_global_root = workspace.path().join(".global-vv-agent");
    let noncanonical_project_root = workspace.path().join("nested/../.vv-agent");
    let noncanonical_global_root = workspace.path().join("nested/../.global-vv-agent");
    std::fs::create_dir_all(workspace.path().join("nested")).expect("nested");
    std::fs::create_dir_all(&canonical_project_root).expect("project root");
    std::fs::create_dir_all(&canonical_global_root).expect("global root");

    let loader = AgentResourceLoader::with_resource_dirs(
        workspace.path(),
        &noncanonical_project_root,
        &noncanonical_global_root,
    );

    assert_eq!(loader.project_resource_dir, canonical_project_root);
    assert_eq!(loader.global_resource_dir, canonical_global_root);
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
fn resource_loader_tracks_python_hook_files_with_rust_diagnostics() {
    let workspace = tempfile::tempdir().expect("workspace");
    let resource_root = workspace.path().join(".vv-agent");
    std::fs::create_dir_all(resource_root.join("hooks/nested")).expect("hooks");
    std::fs::write(resource_root.join("hooks/noop.py"), "HOOK = object()").expect("hook");
    std::fs::write(
        resource_root.join("hooks/nested/index.py"),
        "HOOK = object()",
    )
    .expect("nested hook");

    let mut loader = AgentResourceLoader::with_resource_dirs(
        workspace.path(),
        &resource_root,
        workspace.path().join(".none"),
    );
    let discovered = loader.discover();

    assert_eq!(discovered.hook_files.len(), 2);
    assert_eq!(discovered.hooks, discovered.hook_files);
    assert!(discovered
        .hook_files
        .iter()
        .any(|path| path.ends_with("hooks/noop.py")));
    assert!(discovered
        .hook_files
        .iter()
        .any(|path| path.ends_with("hooks/nested/index.py")));
    assert!(discovered.diagnostics.iter().any(|diagnostic| {
        diagnostic.contains("Python hook file discovered")
            && diagnostic.contains("AgentSDKOptions.runtime_hooks")
    }));
}

#[test]
fn resource_loader_canonicalizes_hook_file_paths_like_python() {
    let workspace = tempfile::tempdir().expect("workspace");
    let resource_root = workspace.path().join(".vv-agent");
    let noncanonical_resource_root = workspace.path().join("nested/../.vv-agent");
    std::fs::create_dir_all(workspace.path().join("nested")).expect("nested");
    std::fs::create_dir_all(resource_root.join("hooks")).expect("hooks");
    std::fs::write(resource_root.join("hooks/noop.py"), "HOOK = object()").expect("hook");

    let mut loader = AgentResourceLoader::with_resource_dirs(
        workspace.path(),
        &noncanonical_resource_root,
        workspace.path().join(".none"),
    );
    let discovered = loader.discover();

    assert_eq!(
        discovered.hook_files,
        vec![resource_root
            .join("hooks/noop.py")
            .to_string_lossy()
            .to_string()]
    );
    assert!(!discovered.hook_files[0].contains(".."));
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
fn sdk_client_new_with_agent_prepares_default_task_like_python() {
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
    assert_python_style_task_id(&task.task_id, "default");
    assert!(task.system_prompt.contains("default inline agent"));
}

#[test]
fn sdk_client_new_with_agents_resolves_only_agent_like_python() {
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

    assert_python_style_task_id(&task.task_id, "researcher");
    assert_eq!(task.user_prompt, "preview only profile");
}

#[test]
fn sdk_prepare_and_run_use_python_style_unique_task_ids() {
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
    assert_python_style_task_id(&first.task_id, "researcher");
    assert_python_style_task_id(&second.task_id, "researcher");
    assert_ne!(first.task_id, second.task_id);

    let inline = client.prepare_task_with_agent(
        AgentDefinition::default_for_model("demo-model"),
        "preview inline",
        "demo-model",
    );
    assert_python_style_task_id(&inline.task_id, "inline");

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
    assert_python_style_task_id(&captured[0], "researcher");
}

fn assert_python_style_task_id(task_id: &str, prefix: &str) {
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

#[test]
fn sdk_prepare_task_resolves_relative_skill_directories_from_workspace_like_python() {
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
fn sdk_prepare_task_clamps_runtime_limits_like_python() {
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
fn sdk_client_query_agent_compatibility_wrapper_like_python() {
    let workspace = tempfile::tempdir().expect("workspace");
    let runtime = AgentRuntime::new(ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "done",
        vec![ToolCall::new(
            "finish",
            "task_finish",
            BTreeMap::from([("message".to_string(), json!("compat-query"))]),
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

    assert_eq!(text, "compat-query");
}

#[test]
fn sdk_client_query_agent_can_return_wait_reason_when_not_strict_like_python() {
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
fn sdk_one_shot_run_can_override_workspace_like_python() {
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

#[test]
fn sdk_module_level_run_with_options_and_agent_helper_like_python() {
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
fn sdk_module_level_query_with_options_and_agent_helper_like_python() {
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
fn sdk_options_tool_registry_factory_runs_custom_tools_like_python() {
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

#[test]
fn sdk_client_run_agent_with_request_passes_shared_state_like_python() {
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
fn sdk_default_run_request_passes_python_one_shot_runtime_controls() {
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
fn sdk_default_query_request_can_return_non_completed_wait_reason_like_python() {
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
fn sdk_module_level_run_request_helper_passes_shared_state_like_python() {
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
fn sdk_client_run_agent_with_request_passes_before_cycle_messages_like_python() {
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
fn sdk_client_run_agent_with_request_passes_interruption_messages_like_python() {
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
fn sdk_options_log_handler_receives_runtime_events_like_python() {
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
fn sdk_options_pass_debug_dump_dir_to_custom_llm_builder_like_python() {
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
