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
