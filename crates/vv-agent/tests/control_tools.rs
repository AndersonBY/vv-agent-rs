use std::collections::BTreeMap;

use serde_json::json;
use vv_agent::{build_default_registry, ToolCall, ToolContext, ToolDirective, ToolResultStatus};

#[test]
fn todo_write_updates_shared_state_and_enforces_single_in_progress() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let result = registry
        .execute(
            &ToolCall::new(
                "todo_1",
                "todo_write",
                BTreeMap::from([(
                    "todos".to_string(),
                    json!([
                        {"title": "a", "status": "in_progress", "priority": "high"},
                        {"title": "b", "status": "in_progress", "priority": "medium"}
                    ]),
                )]),
            ),
            &mut context,
        )
        .expect("todo_write");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert!(result.content.contains("multiple_in_progress_todos"));

    let result = registry
        .execute(
            &ToolCall::new(
                "todo_2",
                "todo_write",
                BTreeMap::from([(
                    "todos".to_string(),
                    json!([
                        {"title": "a", "status": "in_progress", "priority": "high"},
                        {"title": "b", "status": "pending", "priority": "medium"}
                    ]),
                )]),
            ),
            &mut context,
        )
        .expect("todo_write");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(
        context.shared_state["todo_list"][0]["title"].as_str(),
        Some("a")
    );
}

#[test]
fn task_finish_blocks_when_todos_are_incomplete() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    registry
        .execute(
            &ToolCall::new(
                "todo_1",
                "todo_write",
                BTreeMap::from([(
                    "todos".to_string(),
                    json!([{"title": "step1", "status": "pending", "priority": "medium"}]),
                )]),
            ),
            &mut context,
        )
        .expect("todo_write");

    let result = registry
        .execute(
            &ToolCall::new(
                "finish_1",
                "task_finish",
                BTreeMap::from([("message".to_string(), json!("done"))]),
            ),
            &mut context,
        )
        .expect("task_finish");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.directive, ToolDirective::Continue);
    assert!(result.content.contains("todo_incomplete"));
}

#[test]
fn activate_skill_loads_skill_md_and_updates_shared_state() {
    let workspace = tempfile::tempdir().expect("workspace");
    let skill_dir = workspace.path().join("skills/demo");
    std::fs::create_dir_all(&skill_dir).expect("skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        r#"---
name: demo-skill
description: Demo skill description
compatibility: rust tests
allowed-tools: read_file, write_file
metadata:
  owner: agent
---
Use this skill body during execution.
"#,
    )
    .expect("skill md");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    context
        .shared_state
        .insert("available_skills".to_string(), json!(["skills"]));
    context.cycle_index = 3;

    let result = registry
        .execute(
            &ToolCall::new(
                "skill_1",
                "activate_skill",
                BTreeMap::from([
                    ("skill_name".to_string(), json!("demo-skill")),
                    ("reason".to_string(), json!("Need demo behavior")),
                ]),
            ),
            &mut context,
        )
        .expect("activate_skill");

    assert_eq!(result.status, ToolResultStatus::Success);
    let payload: serde_json::Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["status"], "activated");
    assert_eq!(payload["skill_name"], "demo-skill");
    assert_eq!(
        payload["instructions"],
        "Use this skill body during execution."
    );
    assert_eq!(payload["description"], "Demo skill description");
    assert_eq!(payload["compatibility"], "rust tests");
    assert_eq!(payload["allowed_tools"], "read_file, write_file");
    assert_eq!(payload["metadata"]["owner"], "agent");
    assert_eq!(payload["reason"], "Need demo behavior");
    assert_eq!(context.shared_state["active_skills"], json!(["demo-skill"]));
    assert_eq!(
        context.shared_state["skill_activation_log"][0]["cycle_index"],
        3
    );
}

#[test]
fn activate_skill_accepts_inline_entries_and_reports_disallowed_skill() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    context.shared_state.insert(
        "available_skills".to_string(),
        json!([
            {
                "name": "inline-skill",
                "description": "Inline description",
                "instructions": "Inline body"
            }
        ]),
    );

    let result = registry
        .execute(
            &ToolCall::new(
                "skill_inline",
                "activate_skill",
                BTreeMap::from([("skill_name".to_string(), json!("inline-skill"))]),
            ),
            &mut context,
        )
        .expect("activate inline skill");
    assert_eq!(result.status, ToolResultStatus::Success);
    let payload: serde_json::Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["instructions"], "Inline body");

    let result = registry
        .execute(
            &ToolCall::new(
                "skill_denied",
                "activate_skill",
                BTreeMap::from([("skill_name".to_string(), json!("missing"))]),
            ),
            &mut context,
        )
        .expect("activate missing skill");
    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("skill_not_allowed"));
}
