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

#[test]
fn sanitize_for_resume_drops_tool_results_with_empty_ids() {
    for tool_call_id in [None, Some(""), Some(" \n ")] {
        let mut tool_result = Message::tool("unlinked result", "placeholder");
        tool_result.tool_call_id = tool_call_id.map(str::to_string);
        let messages = vec![Message::user("hello"), tool_result];

        assert_eq!(sanitize_for_resume(&messages), vec![Message::user("hello")]);
    }
}

#[test]
fn sanitize_for_resume_drops_unresolved_tool_calls_with_empty_ids() {
    for tool_call_id in ["", " \n "] {
        let messages = vec![
            Message::user("hello"),
            Message {
                tool_calls: vec![ToolCall::new(
                    tool_call_id,
                    "read_file",
                    BTreeMap::from([("path".to_string(), serde_json::json!("README.md"))]),
                )],
                ..Message::assistant("")
            },
        ];

        assert_eq!(sanitize_for_resume(&messages), vec![Message::user("hello")]);
    }
}

#[test]
fn sanitize_for_resume_removes_empty_call_from_mixed_tool_calls() {
    let messages = vec![
        Message {
            tool_calls: vec![
                ToolCall::new(
                    "",
                    "read_file",
                    BTreeMap::from([("path".to_string(), serde_json::json!("missing.md"))]),
                ),
                ToolCall::new(
                    "call-1",
                    "read_file",
                    BTreeMap::from([("path".to_string(), serde_json::json!("README.md"))]),
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

#[test]
fn sanitize_for_resume_matches_tool_call_ids_after_trimming() {
    let messages = vec![
        Message {
            tool_calls: vec![ToolCall::new(
                " call-1 ",
                "read_file",
                BTreeMap::from([("path".to_string(), serde_json::json!("README.md"))]),
            )],
            ..Message::assistant("Working")
        },
        Message::tool("README", "call-1"),
    ];

    assert_eq!(sanitize_for_resume(&messages), messages);
}

fn configured_sub_agent_contract() -> serde_json::Value {
    serde_json::from_str(include_str!("fixtures/parity/configured_sub_agent_v1.json"))
        .expect("configured sub-agent fixture")
}

#[test]
fn sanitize_for_resume_does_not_reuse_an_earlier_result_for_a_later_call() {
    assert_eq!(
        configured_sub_agent_contract()["continuation"]["tool_result_pairing"],
        "immediately_following_assistant_turn"
    );
    let completed_assistant = Message {
        tool_calls: vec![ToolCall::new(
            "reused",
            "read_file",
            BTreeMap::from([("path".to_string(), serde_json::json!("first.md"))]),
        )],
        ..Message::assistant("first")
    };
    let completed_result = Message::tool("first result", "reused");
    let messages = vec![
        completed_assistant.clone(),
        completed_result.clone(),
        Message::user("next"),
        Message {
            tool_calls: vec![ToolCall::new(
                "reused",
                "read_file",
                BTreeMap::from([("path".to_string(), serde_json::json!("second.md"))]),
            )],
            ..Message::assistant("second")
        },
        Message::user("resume"),
    ];

    assert_eq!(
        sanitize_for_resume(&messages),
        vec![
            completed_assistant,
            completed_result,
            Message::user("next"),
            Message::user("resume"),
        ]
    );
}

#[test]
fn sanitize_for_resume_drops_ambiguous_duplicate_ids_and_results() {
    assert_eq!(
        configured_sub_agent_contract()["continuation"]["duplicate_tool_call_id_policy"],
        "drop_ambiguous_call_and_results"
    );
    let messages = vec![
        Message::user("before"),
        Message {
            tool_calls: vec![
                ToolCall::new(
                    "duplicate",
                    "read_file",
                    BTreeMap::from([("path".to_string(), serde_json::json!("a.md"))]),
                ),
                ToolCall::new(
                    "duplicate",
                    "read_file",
                    BTreeMap::from([("path".to_string(), serde_json::json!("b.md"))]),
                ),
            ],
            ..Message::assistant("ambiguous")
        },
        Message::tool("which call?", "duplicate"),
        Message::user("after"),
    ];

    assert_eq!(
        sanitize_for_resume(&messages),
        vec![Message::user("before"), Message::user("after")]
    );
}

#[test]
fn sanitize_for_resume_drops_out_of_order_results_and_unresolved_calls() {
    assert_eq!(
        configured_sub_agent_contract()["continuation"]["out_of_order_tool_result_policy"],
        "drop_orphan_result"
    );
    let messages = vec![
        Message::tool("too early", "late"),
        Message::user("boundary"),
        Message {
            tool_calls: vec![ToolCall::new(
                "late",
                "read_file",
                BTreeMap::from([("path".to_string(), serde_json::json!("late.md"))]),
            )],
            ..Message::assistant("late call")
        },
        Message::user("resume"),
    ];

    assert_eq!(
        sanitize_for_resume(&messages),
        vec![Message::user("boundary"), Message::user("resume")]
    );
}

#[test]
fn sanitize_for_resume_requires_results_in_tool_call_order() {
    assert_eq!(
        configured_sub_agent_contract()["continuation"]["tool_result_order"],
        "same_as_tool_calls"
    );
    let messages = vec![
        Message {
            tool_calls: vec![
                ToolCall::new(
                    "call-a",
                    "read_file",
                    BTreeMap::from([("path".to_string(), serde_json::json!("a.md"))]),
                ),
                ToolCall::new(
                    "call-b",
                    "read_file",
                    BTreeMap::from([("path".to_string(), serde_json::json!("b.md"))]),
                ),
            ],
            ..Message::assistant("Working")
        },
        Message::tool("B", "call-b"),
        Message::tool("A", "call-a"),
        Message::user("resume"),
    ];

    assert_eq!(
        sanitize_for_resume(&messages),
        vec![Message::user("resume")]
    );
}

#[test]
fn sanitize_for_resume_keeps_only_ordered_result_prefix() {
    let messages = vec![
        Message {
            tool_calls: vec![
                ToolCall::new(
                    "call-a",
                    "read_file",
                    BTreeMap::from([("path".to_string(), serde_json::json!("a.md"))]),
                ),
                ToolCall::new(
                    "call-b",
                    "read_file",
                    BTreeMap::from([("path".to_string(), serde_json::json!("b.md"))]),
                ),
                ToolCall::new(
                    "call-c",
                    "read_file",
                    BTreeMap::from([("path".to_string(), serde_json::json!("c.md"))]),
                ),
            ],
            ..Message::assistant("Working")
        },
        Message::tool("A", "call-a"),
        Message::tool("C", "call-c"),
        Message::tool("B", "call-b"),
    ];

    let sanitized = sanitize_for_resume(&messages);

    assert_eq!(sanitized.len(), 2);
    assert_eq!(sanitized[0].tool_calls.len(), 1);
    assert_eq!(sanitized[0].tool_calls[0].id, "call-a");
    assert_eq!(sanitized[1].tool_call_id.as_deref(), Some("call-a"));
}

#[test]
fn sanitize_for_resume_does_not_skip_duplicate_result_to_keep_a_later_pair() {
    assert_eq!(
        configured_sub_agent_contract()["continuation"]["mismatched_tool_result_policy"],
        "retain_ordered_prefix"
    );
    let messages = vec![
        Message {
            tool_calls: vec![
                ToolCall::new("first", "read_file", BTreeMap::new()),
                ToolCall::new("tail", "read_file", BTreeMap::new()),
            ],
            ..Message::assistant("Working")
        },
        Message::tool("first result", "first"),
        Message::tool("duplicate first result", "first"),
        Message::tool("tail result", "tail"),
    ];

    let sanitized = sanitize_for_resume(&messages);

    assert!(sanitized.is_empty());
    assert!(sanitized
        .iter()
        .all(|message| message.role != vv_agent::MessageRole::Tool));
}

#[test]
fn sanitize_for_resume_does_not_skip_duplicate_call_to_keep_a_later_pair() {
    let messages = vec![
        Message {
            tool_calls: vec![
                ToolCall::new("duplicate", "read_file", BTreeMap::new()),
                ToolCall::new("duplicate", "read_file", BTreeMap::new()),
                ToolCall::new("tail", "read_file", BTreeMap::new()),
            ],
            ..Message::assistant("Working")
        },
        Message::tool("ambiguous result", "duplicate"),
        Message::tool("tail result", "tail"),
    ];

    let sanitized = sanitize_for_resume(&messages);

    assert!(sanitized.is_empty());
    assert!(sanitized
        .iter()
        .all(|message| message.role != vv_agent::MessageRole::Tool));
}
