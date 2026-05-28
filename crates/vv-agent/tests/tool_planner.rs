use serde_json::json;
use vv_agent::runtime::freeze_dynamic_tool_schema_hints;
use vv_agent::runtime::tool_planner::{plan_tool_names, plan_tool_schemas};
use vv_agent::{build_default_registry, AgentTask, SubAgentConfig};

#[test]
fn planned_tool_schemas_respect_task_capability_flags() {
    let registry = build_default_registry();
    let mut task = AgentTask::new("task_planner", "dummy", "sys", "user");
    task.allow_interruption = false;
    task.use_workspace = false;

    let names = registry.planned_tool_names(&task);
    let free_names = plan_tool_names(&task, None);

    assert_eq!(names, vec!["task_finish".to_string()]);
    assert_eq!(free_names, names);
}

#[test]
fn planned_tool_schemas_include_todo_write_workspace_tools() {
    let registry = build_default_registry();
    let task = AgentTask::new("task_planner", "dummy", "sys", "user");

    let names = registry.planned_tool_names(&task);

    assert!(names.contains(&"todo_write".to_string()));
    assert!(registry
        .planned_openai_schemas(&task)
        .iter()
        .any(|schema| schema["function"]["name"] == "todo_write"));
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
    let free_schemas = plan_tool_schemas(&registry, &task, None);
    let names = schemas
        .iter()
        .filter_map(|schema| schema["function"]["name"].as_str().map(str::to_string))
        .collect::<Vec<_>>();
    let free_names = free_schemas
        .iter()
        .filter_map(|schema| schema["function"]["name"].as_str().map(str::to_string))
        .collect::<Vec<_>>();

    assert!(!names.contains(&"read_file".to_string()));
    assert!(!names.contains(&"write_file".to_string()));
    assert!(names.contains(&"task_finish".to_string()));
    assert_eq!(free_names, names);
}

#[test]
fn planned_tool_names_keep_unregistered_extra_tools() {
    let registry = build_default_registry();
    let mut task = AgentTask::new("task_planner", "dummy", "sys", "user");
    task.extra_tool_names
        .push("external_custom_tool".to_string());

    let names = registry.planned_tool_names(&task);
    let schema_names = registry
        .planned_openai_schemas(&task)
        .iter()
        .filter_map(|schema| schema["function"]["name"].as_str().map(str::to_string))
        .collect::<Vec<_>>();

    assert!(names.contains(&"external_custom_tool".to_string()));
    assert!(!schema_names.contains(&"external_custom_tool".to_string()));
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

#[test]
fn freeze_dynamic_tool_schema_hints_caches_computer_shell_hint() {
    let mut task = AgentTask::new("task_planner", "dummy", "sys", "user");
    task.agent_type = Some("computer".to_string());
    task.metadata
        .insert("bash_shell".to_string(), json!("bash"));

    freeze_dynamic_tool_schema_hints(&mut task);

    let cached = task
        .metadata
        .get("_vv_agent_bash_runtime_hint")
        .and_then(|value| value.as_str())
        .expect("cached bash hint");
    assert!(cached.contains("Runtime shell hint:"));
    assert!(cached.contains("bash"));
}

#[test]
fn freeze_dynamic_tool_schema_hints_preserves_existing_shell_hint() {
    let mut task = AgentTask::new("task_planner", "dummy", "sys", "user");
    task.agent_type = Some("computer".to_string());
    task.metadata.insert(
        "_vv_agent_bash_runtime_hint".to_string(),
        json!("Runtime shell hint: cached before dispatch."),
    );
    task.metadata.insert(
        "bash_shell".to_string(),
        json!("definitely-not-installed-shell"),
    );

    freeze_dynamic_tool_schema_hints(&mut task);

    assert_eq!(
        task.metadata["_vv_agent_bash_runtime_hint"],
        json!("Runtime shell hint: cached before dispatch.")
    );
}

#[test]
fn freeze_dynamic_tool_schema_hints_also_caches_explicit_bash_tool() {
    let mut task = AgentTask::new("task_planner", "dummy", "sys", "user");
    task.agent_type = Some("assistant".to_string());
    task.extra_tool_names.push("bash".to_string());

    freeze_dynamic_tool_schema_hints(&mut task);

    assert!(task.metadata.contains_key("_vv_agent_bash_runtime_hint"));
}

#[test]
fn freeze_dynamic_tool_schema_hints_skips_tasks_without_bash_access() {
    let mut task = AgentTask::new("task_planner", "dummy", "sys", "user");

    freeze_dynamic_tool_schema_hints(&mut task);

    assert!(!task.metadata.contains_key("_vv_agent_bash_runtime_hint"));
}
