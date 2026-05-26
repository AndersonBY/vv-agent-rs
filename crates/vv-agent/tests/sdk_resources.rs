use serde_json::json;
use vv_agent::AgentResourceLoader;

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
