use serde_json::json;
use vv_agent::{build_default_registry, AgentTask, SubAgentConfig};

#[test]
fn planned_tool_schemas_respect_task_capability_flags() {
    let registry = build_default_registry();
    let mut task = AgentTask::new("task_planner", "dummy", "sys", "user");
    task.allow_interruption = false;
    task.use_workspace = false;

    let names = registry.planned_tool_names(&task);

    assert_eq!(names, vec!["task_finish".to_string()]);
}

#[test]
fn planned_tool_schemas_add_computer_sub_agent_skill_and_multimodal_tools() {
    let registry = build_default_registry();
    let mut task = AgentTask::new("task_planner", "dummy", "sys", "user");
    task.agent_type = Some("computer".to_string());
    task.native_multimodal = true;
    task.sub_agents.insert(
        "research-sub".to_string(),
        SubAgentConfig::new("kimi-k2.5", "collect context"),
    );
    task.metadata.insert(
        "available_skills".to_string(),
        json!([{"name": "demo", "description": "Demo"}]),
    );

    let names = registry.planned_tool_names(&task);

    assert!(names.contains(&"bash".to_string()));
    assert!(names.contains(&"check_background_command".to_string()));
    assert!(names.contains(&"create_sub_task".to_string()));
    assert!(names.contains(&"sub_task_status".to_string()));
    assert!(names.contains(&"read_image".to_string()));
    assert!(names.contains(&"activate_skill".to_string()));
}

#[test]
fn planned_tool_schemas_exclude_tools() {
    let registry = build_default_registry();
    let mut task = AgentTask::new("task_planner", "dummy", "sys", "user");
    task.exclude_tools = vec!["read_file".to_string(), "write_file".to_string()];

    let schemas = registry.planned_openai_schemas(&task);
    let names = schemas
        .iter()
        .filter_map(|schema| schema["function"]["name"].as_str().map(str::to_string))
        .collect::<Vec<_>>();

    assert!(!names.contains(&"read_file".to_string()));
    assert!(!names.contains(&"write_file".to_string()));
    assert!(names.contains(&"task_finish".to_string()));
}

#[test]
fn planned_tool_schemas_inject_runtime_shell_hint_for_bash() {
    let registry = build_default_registry();
    let mut task = AgentTask::new("task_planner", "dummy", "sys", "user");
    task.agent_type = Some("computer".to_string());
    task.metadata
        .insert("bash_shell".to_string(), json!("powershell"));
    task.metadata.insert(
        "windows_shell_priority".to_string(),
        json!(["git-bash", "powershell", "cmd"]),
    );

    let schemas = registry.planned_openai_schemas(&task);
    let bash = schemas
        .iter()
        .find(|schema| schema["function"]["name"] == "bash")
        .expect("bash schema");
    let description = bash["function"]["description"]
        .as_str()
        .expect("description");

    assert!(description.contains("Runtime shell hint:"));
    assert!(description.contains("powershell"));
    assert!(description.contains("-NoProfile"));
}

#[test]
fn planned_tool_schemas_reports_invalid_windows_shell_priority_config() {
    let registry = build_default_registry();
    let mut task = AgentTask::new("task_planner", "dummy", "sys", "user");
    task.agent_type = Some("computer".to_string());
    task.metadata.insert(
        "windows_shell_priority".to_string(),
        json!("git-bash,powershell,cmd"),
    );

    let schemas = registry.planned_openai_schemas(&task);
    let bash = schemas
        .iter()
        .find(|schema| schema["function"]["name"] == "bash")
        .expect("bash schema");
    let description = bash["function"]["description"]
        .as_str()
        .expect("description");

    assert!(description.contains("Runtime shell hint:"));
    assert!(description.contains("invalid shell config"));
}
