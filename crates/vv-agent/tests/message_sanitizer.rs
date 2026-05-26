use std::collections::BTreeMap;

use vv_agent::memory::sanitize_for_resume;
use vv_agent::{Message, ToolCall};

#[test]
fn sanitize_for_resume_drops_blank_assistant_messages() {
    let messages = vec![Message::user("hello"), Message::assistant("   ")];

    let sanitized = sanitize_for_resume(&messages);

    assert_eq!(sanitized, vec![Message::user("hello")]);
}

#[test]
fn sanitize_for_resume_drops_thinking_only_messages() {
    let messages = vec![
        Message {
            reasoning_content: Some("thinking".to_string()),
            ..Message::assistant("")
        },
        Message::user("continue"),
    ];

    let sanitized = sanitize_for_resume(&messages);

    assert_eq!(sanitized, vec![Message::user("continue")]);
}

#[test]
fn sanitize_for_resume_drops_orphan_tool_results() {
    let messages = vec![
        Message::assistant("done"),
        Message::tool("result", "orphan-call"),
    ];

    let sanitized = sanitize_for_resume(&messages);

    assert_eq!(sanitized, vec![Message::assistant("done")]);
}

#[test]
fn sanitize_for_resume_drops_unresolved_tail_tool_use() {
    let messages = vec![Message {
        tool_calls: vec![ToolCall::new(
            "call-1",
            "read_file",
            BTreeMap::from([("path".to_string(), serde_json::json!("README.md"))]),
        )],
        ..Message::assistant("")
    }];

    let sanitized = sanitize_for_resume(&messages);

    assert!(sanitized.is_empty());
}

#[test]
fn sanitize_for_resume_trims_only_unresolved_tool_calls() {
    let messages = vec![
        Message {
            tool_calls: vec![
                ToolCall::new(
                    "call-1",
                    "read_file",
                    BTreeMap::from([("path".to_string(), serde_json::json!("README.md"))]),
                ),
                ToolCall::new(
                    "call-2",
                    "write_file",
                    BTreeMap::from([("path".to_string(), serde_json::json!("notes.md"))]),
                ),
            ],
            ..Message::assistant("Working")
        },
        Message::tool("README", "call-1"),
    ];

    let sanitized = sanitize_for_resume(&messages);

    assert_eq!(sanitized.len(), 2);
    assert_eq!(sanitized[0].tool_calls.len(), 1);
    assert_eq!(sanitized[0].tool_calls[0].id, "call-1");
}
