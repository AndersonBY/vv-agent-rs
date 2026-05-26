use std::collections::BTreeMap;

use vv_agent::memory::{microcompact, MicrocompactConfig, CLEARED_MARKER};
use vv_agent::{Message, ToolCall};

fn build_messages() -> Vec<Message> {
    vec![
        Message::system("sys"),
        Message::user("start"),
        Message {
            tool_calls: vec![ToolCall::new("call_old", "read_file", BTreeMap::new())],
            ..Message::assistant("old tool call")
        },
        Message::tool("x".repeat(800), "call_old"),
        Message::assistant("cycle two"),
        Message::user("continue"),
        Message {
            tool_calls: vec![ToolCall::new("call_recent", "read_file", BTreeMap::new())],
            ..Message::assistant("recent tool call")
        },
        Message::tool("y".repeat(800), "call_recent"),
    ]
}

#[test]
fn microcompact_clears_only_old_compactable_tool_results() {
    let (messages, cleared) = microcompact(
        &build_messages(),
        4,
        &MicrocompactConfig {
            keep_recent_cycles: 2,
            min_result_length: 500,
            ..MicrocompactConfig::default()
        },
    );

    assert_eq!(cleared, 1);
    let tool_messages = messages
        .iter()
        .filter(|message| message.tool_call_id.is_some())
        .collect::<Vec<_>>();
    assert_eq!(tool_messages[0].content, CLEARED_MARKER);
    assert_ne!(tool_messages[1].content, CLEARED_MARKER);
    assert_eq!(tool_messages[0].metadata["microcompacted"], true);
}

#[test]
fn microcompact_respects_min_length_and_tool_filter() {
    let mut messages = build_messages();
    messages[2] = Message {
        tool_calls: vec![ToolCall::new("call_old", "custom_tool", BTreeMap::new())],
        ..Message::assistant("old custom call")
    };
    let (compacted, cleared) = microcompact(
        &messages,
        4,
        &MicrocompactConfig {
            keep_recent_cycles: 2,
            min_result_length: 500,
            ..MicrocompactConfig::default()
        },
    );
    assert_eq!(cleared, 0);
    assert_ne!(compacted[3].content, CLEARED_MARKER);

    let mut boundary = build_messages();
    boundary[3] = Message::tool("x".repeat(500), "call_old");
    let (compacted, cleared) = microcompact(
        &boundary,
        4,
        &MicrocompactConfig {
            keep_recent_cycles: 2,
            min_result_length: 500,
            ..MicrocompactConfig::default()
        },
    );
    assert_eq!(cleared, 0);
    assert_ne!(compacted[3].content, CLEARED_MARKER);
}

#[test]
fn microcompact_clamps_external_cycle_to_inferred_window() {
    let (messages, cleared) = microcompact(
        &build_messages(),
        15,
        &MicrocompactConfig {
            keep_recent_cycles: 1,
            min_result_length: 500,
            ..MicrocompactConfig::default()
        },
    );

    assert_eq!(cleared, 1);
    let tool_messages = messages
        .iter()
        .filter(|message| message.tool_call_id.is_some())
        .collect::<Vec<_>>();
    assert_eq!(tool_messages[0].content, CLEARED_MARKER);
    assert_ne!(tool_messages[1].content, CLEARED_MARKER);
}
