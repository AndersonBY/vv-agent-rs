use super::*;

#[test]
fn memory_manager_does_not_compact_small_history() {
    let mut manager = MemoryManager::new(MemoryManagerConfig {
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
fn memory_manager_appends_agent_warning_before_compaction() {
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        model_context_window: 120,
        reserved_output_tokens: 10,
        autocompact_buffer_tokens: 10,
        warning_threshold_percentage: 90,
        include_memory_warning: true,
        language: "en-US".to_string(),
        ..MemoryManagerConfig::default()
    });
    let messages = vec![Message::system("sys"), Message::user("hello")];

    let (warned, changed) =
        manager.compact_for_cycle_with_usage(&messages, 0, false, Some(90), None);

    assert!(changed);
    assert_eq!(warned.len(), 3);
    assert!(warned[2]
        .content
        .contains("The current memory usage has exceeded 90%."));

    let (deduped, changed) =
        manager.compact_for_cycle_with_usage(&warned, 0, false, Some(90), None);

    assert!(!changed);
    assert_eq!(deduped, warned);
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

#[test]
fn memory_manager_exposes_agent_threshold_properties() {
    let manager = MemoryManager::new(MemoryManagerConfig {
        compact_threshold: 100_000,
        model_context_window: 64_000,
        reserved_output_tokens: 8_000,
        autocompact_buffer_tokens: 6_000,
        warning_threshold_percentage: 80,
        microcompaction_policy: MicrocompactionPolicy::new(0.5, 0.4, 3, 500).expect("policy"),
        ..MemoryManagerConfig::default()
    });

    assert_eq!(manager.effective_context_window(), 56_000);
    assert_eq!(manager.autocompact_threshold(), 50_000);
    assert_eq!(manager.warning_threshold(), 40_000);
    assert_eq!(manager.microcompact_trigger_threshold(), 25_000);
}

#[test]
fn memory_manager_uses_microcompact_before_full_summary() {
    let backend = Arc::new(vv_agent::MemoryWorkspaceBackend::default());
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        compact_threshold: 1_000,
        model_context_window: 4_000,
        reserved_output_tokens: 0,
        autocompact_buffer_tokens: 0,
        microcompaction_policy: MicrocompactionPolicy::new(0.01, 0.005, 1, 200).expect("policy"),
        keep_recent_messages: 2,
        ..MemoryManagerConfig::default()
    })
    .with_workspace_backend(backend)
    .with_recovery_tool_available(true);
    let messages = vec![
        Message::system("sys"),
        Message::user("start"),
        Message {
            tool_calls: vec![ToolCall::new("call_old", "read_file", BTreeMap::new())],
            ..Message::assistant("old tool call")
        },
        Message::tool("large result ".repeat(600), "call_old"),
        Message::assistant("recent reply"),
        Message::user("latest ask"),
    ];

    let (compacted, changed) = manager.compact_for_cycle(&messages, 3, false);

    assert!(changed);
    assert!(compacted
        .iter()
        .any(|message| message.content.starts_with(TOOL_RESULT_COMPACT_MARKER)));
    assert!(compacted
        .iter()
        .all(|message| !message.content.contains("<Compressed Agent Memory>")));
}
