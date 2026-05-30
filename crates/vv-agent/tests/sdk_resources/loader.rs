use super::*;

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
fn resource_loader_force_reload_refreshes_cached_resources() {
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
fn sdk_options_resource_loader_discovers_from_custom_roots() {
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
fn resource_loader_expands_home_paths() {
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
fn resource_loader_canonicalizes_relative_skill_directories() {
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
fn resource_loader_parses_agent_booleans_with_agent_truthiness() {
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
fn resource_loader_clamps_numeric_runtime_limits_like_sdk_task_preparation() {
    let workspace = tempfile::tempdir().expect("workspace");
    let resource_root = workspace.path().join(".vv-agent");
    std::fs::create_dir_all(&resource_root).expect("resource root");
    std::fs::write(
        resource_root.join("agents.json"),
        json!({
            "profiles": {
                "limits": {
                    "description": "limit profile",
                    "model": "demo-model",
                    "max_cycles": -3,
                    "memory_compact_threshold": -20,
                    "memory_threshold_percentage": 1000,
                    "sub_agents": {
                        "worker": {
                            "description": "worker profile",
                            "model": "demo-child",
                            "max_cycles": -5
                        }
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
    let agent = &discovered.agents["limits"];

    assert_eq!(agent.max_cycles, 1);
    assert_eq!(agent.memory_compact_threshold, 1);
    assert_eq!(agent.memory_threshold_percentage, 100);
    assert_eq!(agent.sub_agents["worker"].max_cycles, 1);
}

#[test]
fn resource_loader_stringifies_shell_lists_and_env() {
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
fn resource_loader_canonicalizes_root_skill_directory() {
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
fn resource_loader_canonicalizes_resource_roots() {
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
fn resource_loader_reports_invalid_agent_profiles() {
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
fn resource_loader_ignores_unmanaged_resource_directories() {
    let workspace = tempfile::tempdir().expect("workspace");
    let resource_root = workspace.path().join(".vv-agent");
    std::fs::create_dir_all(resource_root.join("scratch/nested")).expect("scratch");
    std::fs::write(resource_root.join("scratch/ignored.txt"), "ignored").expect("ignored file");
    std::fs::write(resource_root.join("scratch/nested/ignored.txt"), "ignored")
        .expect("nested ignored file");

    let mut loader = AgentResourceLoader::with_resource_dirs(
        workspace.path(),
        &resource_root,
        workspace.path().join(".none"),
    );
    let discovered = loader.discover();

    let debug = format!("{discovered:?}");
    assert!(!debug.contains("scratch"));
    assert!(!debug.contains("ignored.txt"));
    assert!(discovered.diagnostics.is_empty());
}
