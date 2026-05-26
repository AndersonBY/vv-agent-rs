use std::collections::BTreeMap;

use serde_json::{json, Value};
use vv_agent::{
    build_default_registry, memory::token_utils::compute_compaction_threshold, MemoryManager,
    MemoryManagerConfig, Message, ToolCall, ToolContext, ToolResultStatus,
};

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

#[test]
fn memory_manager_compacts_to_original_request_and_summary_block() {
    let manager = MemoryManager::new(MemoryManagerConfig {
        compact_threshold: 10,
        model_context_window: 80,
        reserved_output_tokens: 10,
        autocompact_buffer_tokens: 0,
        keep_recent_messages: 3,
        model: "demo".to_string(),
        summary_event_limit: 5,
    });
    let messages = vec![
        Message::system("system"),
        Message::user("original user request"),
        Message::assistant("working"),
        Message::tool("large tool output ".repeat(120), "tool_1"),
        Message::assistant("current state"),
    ];

    let (compacted, changed) = manager.compact(&messages, false);

    assert!(changed);
    assert_eq!(compacted.len(), 2);
    assert_eq!(compacted[0].content, "system");
    assert!(compacted[1]
        .content
        .contains("<Original User Request>\noriginal user request"));
    assert!(compacted[1].content.contains("<Compressed Agent Memory>"));
    assert!(compacted[1].content.contains("\"summary_version\":\"2.0\""));
    assert!(compacted[1].content.contains("current state"));
}

#[test]
fn memory_manager_does_not_compact_small_history() {
    let manager = MemoryManager::new(MemoryManagerConfig {
        compact_threshold: 10_000,
        model_context_window: 20_000,
        reserved_output_tokens: 100,
        autocompact_buffer_tokens: 0,
        ..MemoryManagerConfig::default()
    });
    let messages = vec![Message::system("system"), Message::user("small")];

    let (compacted, changed) = manager.compact(&messages, false);

    assert!(!changed);
    assert_eq!(compacted, messages);
}

#[test]
fn memory_threshold_uses_configured_and_model_derived_ceiling() {
    assert_eq!(
        compute_compaction_threshold(128_000, 200_000, 16_000, 13_000),
        128_000
    );
    assert_eq!(
        compute_compaction_threshold(128_000, 60_000, 10_000, 5_000),
        45_000
    );
    assert_eq!(
        compute_compaction_threshold(0, 60_000, 10_000, 5_000),
        45_000
    );
}
