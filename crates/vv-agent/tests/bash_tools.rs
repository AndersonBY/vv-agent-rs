use std::collections::BTreeMap;

use serde_json::json;
use vv_agent::{build_default_registry, ToolCall, ToolContext, ToolResultStatus};

#[test]
fn bash_tool_executes_command_in_workspace() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let result = registry
        .execute(
            &ToolCall::new(
                "bash_1",
                "bash",
                BTreeMap::from([("command".to_string(), json!("echo hello"))]),
            ),
            &mut context,
        )
        .expect("bash tool");

    assert_eq!(result.status, ToolResultStatus::Success);
    assert!(result.content.contains("\"exit_code\":0"));
    assert!(result.content.contains("hello"));
    assert!(!result.content.contains("\"command\""));
}

#[test]
fn bash_tool_blocks_dangerous_command() {
    let workspace = tempfile::tempdir().expect("workspace");
    let registry = build_default_registry();
    let mut context = ToolContext::new(workspace.path());

    let result = registry
        .execute(
            &ToolCall::new(
                "bash_1",
                "bash",
                BTreeMap::from([("command".to_string(), json!("rm -rf /"))]),
            ),
            &mut context,
        )
        .expect("bash tool");

    assert_eq!(result.status, ToolResultStatus::Error);
    assert_eq!(result.error_code.as_deref(), Some("dangerous_command"));
}
