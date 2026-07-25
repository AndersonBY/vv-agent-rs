use serde_json::json;
use vv_agent::build_default_registry;

use super::helpers::{description, property_description};

const BUILTIN_TOOL_NAMES: [&str; 15] = [
    "task_finish",
    "ask_user",
    "activate_skill",
    "todo_write",
    "find_files",
    "file_info",
    "read_file",
    "write_file",
    "edit_file",
    "search_files",
    "bash",
    "check_background_command",
    "create_sub_task",
    "sub_task_status",
    "read_image",
];

#[test]
fn default_tool_specs_use_the_canonical_schema_descriptions() {
    let registry = build_default_registry();

    for tool_name in BUILTIN_TOOL_NAMES {
        let spec = registry.get(tool_name).expect("tool spec");
        let schema_description = description(&registry, tool_name);
        assert_eq!(spec.description, schema_description, "{tool_name}");
        assert!(!schema_description.trim().is_empty(), "{tool_name}");
        assert!(
            schema_description.chars().count() <= 320,
            "{tool_name} reintroduced an oversized model-visible description"
        );
    }
    assert!(!registry.has_tool("compress_memory"));
}

#[test]
fn default_tool_schema_order_matches_the_canonical_runtime_surface() {
    let registry = build_default_registry();
    let names = registry
        .list_openai_schemas(None)
        .expect("default schemas")
        .into_iter()
        .map(|schema| {
            schema["function"]["name"]
                .as_str()
                .expect("schema name")
                .to_string()
        })
        .collect::<Vec<_>>();

    assert_eq!(names, BUILTIN_TOOL_NAMES);
}

#[test]
fn bounded_result_recovery_is_visible_in_the_relevant_tool_schemas() {
    let registry = build_default_registry();

    assert!(description(&registry, "bash").contains("head/tail preview"));
    assert!(description(&registry, "bash").contains("artifact path"));
    assert!(description(&registry, "check_background_command").contains("preview-and-artifact"));
    assert!(description(&registry, "read_file").contains("returned cursor"));
    assert!(description(&registry, "read_file").contains("stale cursors are rejected"));
    assert_eq!(
        property_description(&registry, "read_file", "cursor"),
        "Continuation state returned by a previous read of this path."
    );

    let read_file = registry.get_schema("read_file").expect("read_file schema");
    let cursor = &read_file["function"]["parameters"]["properties"]["cursor"];
    assert_eq!(
        cursor["required"],
        json!(["kind", "offset_chars", "path", "sha256"])
    );
    assert_eq!(cursor["properties"]["kind"]["const"], "read_file");
    assert_eq!(cursor["properties"]["offset_chars"]["minimum"], 0);
    assert_eq!(cursor["properties"]["sha256"]["pattern"], "^[0-9a-f]{64}$");
}
