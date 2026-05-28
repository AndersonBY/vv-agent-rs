use std::collections::BTreeMap;

use serde_json::json;
use vv_agent::{build_default_registry, ToolCall, ToolContext, ToolDirective, ToolResultStatus};

#[test]
fn todo_handlers_expose_python_style_read_and_write_functions() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut context = ToolContext::new(workspace.path());
    let empty_args = BTreeMap::new();

    let read_empty = vv_agent::tools::handlers::todo::todo_read(&mut context, &empty_args);
    assert_eq!(read_empty.status, ToolResultStatus::Success);
    let read_empty_payload: serde_json::Value =
        serde_json::from_str(&read_empty.content).expect("read empty payload");
    assert_eq!(read_empty_payload["action"], json!("read"));
    assert_eq!(read_empty_payload["count"], json!(0));
    assert_eq!(context.shared_state["todo_list"], json!([]));

    let read_empty_from_handlers = vv_agent::tools::handlers::todo_read(&mut context, &empty_args);
    assert_eq!(read_empty_from_handlers.status, ToolResultStatus::Success);

    let write_result = vv_agent::tools::handlers::todo::todo_write(
        &mut context,
        &BTreeMap::from([(
            "todos".to_string(),
            json!([{"title": "ship parity", "status": "completed", "priority": "high"}]),
        )]),
    );
    assert_eq!(write_result.status, ToolResultStatus::Success);

    let read_written = vv_agent::tools::handlers::todo::todo_read(&mut context, &empty_args);
    let read_written_payload: serde_json::Value =
        serde_json::from_str(&read_written.content).expect("read written payload");
    assert_eq!(read_written_payload["action"], json!("read"));
    assert_eq!(read_written_payload["count"], json!(1));
    assert_eq!(
        read_written_payload["todos"][0]["title"],
        json!("ship parity")
    );
}

#[test]
fn handler_common_helpers_match_python_module() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut context = ToolContext::new(workspace.path());
    let common = vv_agent::tools::handlers::common::to_json(&json!({"text": "你好"}));
    assert_eq!(common, r#"{"text":"你好"}"#);
    assert!(vv_agent::tools::handlers::common::is_string_keyed_dict(
        &json!({"a": 1})
    ));
    assert!(!vv_agent::tools::handlers::common::is_string_keyed_dict(
        &json!(["a"])
    ));

    let todos = vv_agent::tools::handlers::common::get_todo_list(&mut context.shared_state);
    todos.push(json!({"title": "existing", "done": true}));
    assert_eq!(
        context.shared_state["todo_list"][0]["title"],
        json!("existing")
    );

    let normalized = vv_agent::tools::handlers::common::normalize_todo_items(&json!([
        {"title": "  keep  ", "done": 1},
        {"title": " "},
        "skip"
    ]));
    assert_eq!(normalized, vec![json!({"title": "keep", "done": true})]);

    let resolved = vv_agent::tools::handlers::common::resolve_workspace_path(&context, "notes.md")
        .expect("resolved path");
    assert!(resolved.ends_with("notes.md"));
}

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
fn todo_write_rejects_invalid_payloads_like_python() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let result = registry
        .execute(
            &ToolCall::new(
                "todo_invalid_payload",
                "todo_write",
                BTreeMap::from([("todos".to_string(), json!("not an array"))]),
            ),
            &mut context,
        )
        .expect("todo_write");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("invalid_todos_payload"));

    let result = registry
        .execute(
            &ToolCall::new(
                "todo_missing_title",
                "todo_write",
                BTreeMap::from([("todos".to_string(), json!([{"status": "pending"}]))]),
            ),
            &mut context,
        )
        .expect("todo_write");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("todo_title_required"));

    let result = registry
        .execute(
            &ToolCall::new(
                "todo_bad_status",
                "todo_write",
                BTreeMap::from([(
                    "todos".to_string(),
                    json!([{"title": "step", "status": "blocked"}]),
                )]),
            ),
            &mut context,
        )
        .expect("todo_write");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("invalid_todo_status"));
}

#[test]
fn todo_write_generates_python_style_ids_timestamps_and_preserves_created_at() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let result = registry
        .execute(
            &ToolCall::new(
                "todo_create",
                "todo_write",
                BTreeMap::from([("todos".to_string(), json!([{"title": " Draft plan " }]))]),
            ),
            &mut context,
        )
        .expect("todo_write");

    assert_eq!(result.status, ToolResultStatus::Success);
    let payload: serde_json::Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["action"], "write");
    assert_eq!(payload["count"], 1);
    let item = payload["todos"][0].as_object().expect("todo item");
    let generated_id = item["id"].as_str().expect("id").to_string();
    assert_eq!(generated_id.len(), 8);
    assert_eq!(item["title"], "Draft plan");
    assert_eq!(item["status"], "pending");
    assert_eq!(item["priority"], "medium");
    let created_at = item["created_at"].as_str().expect("created_at").to_string();
    assert!(!created_at.is_empty());
    assert!(!item["updated_at"].as_str().expect("updated_at").is_empty());

    let result = registry
        .execute(
            &ToolCall::new(
                "todo_update",
                "todo_write",
                BTreeMap::from([(
                    "todos".to_string(),
                    json!([{
                        "id": generated_id,
                        "title": "Draft plan",
                        "status": "completed",
                        "priority": "high"
                    }]),
                )]),
            ),
            &mut context,
        )
        .expect("todo_write");

    assert_eq!(result.status, ToolResultStatus::Success);
    let updated_payload: serde_json::Value =
        serde_json::from_str(&result.content).expect("updated payload");
    assert_eq!(updated_payload["todos"][0]["created_at"], created_at);
    assert_eq!(
        context.shared_state["todo_list"][0]["created_at"],
        json!(created_at)
    );
    assert_eq!(context.shared_state["todo_list"][0]["status"], "completed");
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
fn ask_user_returns_python_style_selection_metadata_and_dedupes_options() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let result = registry
        .execute(
            &ToolCall::new(
                "ask_1",
                "ask_user",
                BTreeMap::from([
                    ("question".to_string(), json!("Choose")),
                    ("options".to_string(), json!(["A", "B", "B", ""])),
                    ("selection_type".to_string(), json!("multi")),
                    ("allow_custom_options".to_string(), json!(true)),
                ]),
            ),
            &mut context,
        )
        .expect("ask_user");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(result.directive, ToolDirective::WaitUser);
    let payload: serde_json::Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["question"], "Choose");
    assert_eq!(payload["selection_type"], "multi");
    assert_eq!(payload["allow_custom_options"], true);
    assert_eq!(payload["options"], json!(["A", "B"]));
    assert_eq!(result.metadata["options"], json!(["A", "B"]));
}

#[test]
fn control_tools_coerce_scalar_fields_like_python() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let ask_result = registry
        .execute(
            &ToolCall::new(
                "ask_scalar",
                "ask_user",
                BTreeMap::from([
                    ("question".to_string(), json!(123)),
                    ("selection_type".to_string(), json!(false)),
                    ("options".to_string(), json!([1, true, 1, null])),
                ]),
            ),
            &mut context,
        )
        .expect("ask_user");

    assert_eq!(ask_result.status, ToolResultStatus::Success);
    let ask_payload: serde_json::Value =
        serde_json::from_str(&ask_result.content).expect("ask payload");
    assert_eq!(ask_payload["question"], "123");
    assert_eq!(ask_payload["selection_type"], "single");
    assert_eq!(ask_payload["options"], json!(["1", "true"]));

    let finish_result = registry
        .execute(
            &ToolCall::new(
                "finish_scalar",
                "task_finish",
                BTreeMap::from([("message".to_string(), json!(456))]),
            ),
            &mut context,
        )
        .expect("task_finish");

    assert_eq!(finish_result.status, ToolResultStatus::Success);
    let finish_payload: serde_json::Value =
        serde_json::from_str(&finish_result.content).expect("finish payload");
    assert_eq!(finish_payload["message"], "456");
    assert_eq!(finish_result.metadata["final_message"], json!("456"));
}

#[test]
fn activate_skill_loads_skill_md_and_updates_shared_state() {
    let workspace = tempfile::tempdir().expect("workspace");
    let skill_dir = workspace.path().join("skills/demo-skill");
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
