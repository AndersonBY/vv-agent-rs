use std::collections::BTreeMap;

use serde_json::{json, Value};
use vv_agent::{build_default_registry, ToolCall, ToolContext, ToolResultStatus};

#[test]
fn compress_memory_writes_note_to_shared_state() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());
    context.cycle_index = 3;

    let result = registry
        .execute(
            &ToolCall::new(
                "mem_1",
                "compress_memory",
                BTreeMap::from([(
                    "core_information".to_string(),
                    json!("current decision and progress"),
                )]),
            ),
            &mut context,
        )
        .expect("compress_memory");

    assert_eq!(result.status, ToolResultStatus::Success);
    let payload: Value = serde_json::from_str(&result.content).expect("payload");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["saved_notes"], 1);
    assert_eq!(
        context.shared_state["memory_notes"][0]["core_information"].as_str(),
        Some("current decision and progress")
    );
    assert_eq!(context.shared_state["memory_notes"][0]["cycle_index"], 3);
}
