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
                    "bash_env": {"VV_AGENT_RESOURCE_ENV": "resource"},
                    "system_prompt_template": "research"
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
