use super::*;

#[test]
fn memory_manager_compacts_to_original_request_and_summary_block() {
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        compact_threshold: 10,
        model_context_window: 80,
        reserved_output_tokens: 10,
        autocompact_buffer_tokens: 0,
        keep_recent_messages: 3,
        model: "demo".to_string(),
        summary_event_limit: 5,
        ..MemoryManagerConfig::default()
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
fn memory_manager_uses_summary_callback_and_normalizes_output() {
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        compact_threshold: 10,
        model_context_window: 80,
        reserved_output_tokens: 10,
        autocompact_buffer_tokens: 0,
        language: "en-US".to_string(),
        summary_backend: Some("summary-backend".to_string()),
        summary_model: Some("summary-model".to_string()),
        summary_callback: Some(Arc::new(|prompt, backend, model| {
            assert!(prompt.contains("<Conversation History>"));
            assert_eq!(backend, Some("summary-backend"));
            assert_eq!(model, Some("summary-model"));
            Some(
                r#"<analysis>private notes</analysis>
<summary>{"summary_version":"2.0","original_user_messages":["from callback"],"current_work_state":"callback summary"}</summary>"#
                    .to_string(),
            )
        })),
        ..MemoryManagerConfig::default()
    });
    let messages = vec![
        Message::system("system"),
        Message::user("original request"),
        Message::assistant("assistant progress"),
    ];

    let (compacted, changed) = manager.compact(&messages, true);

    assert!(changed);
    assert!(compacted[1].content.contains("\"from callback\""));
    assert!(compacted[1].content.contains("callback summary"));
    assert!(!compacted[1].content.contains("<analysis>"));
    assert!(!compacted[1].content.contains("<summary>"));
}

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
fn memory_manager_uses_provider_tokens_and_recent_tool_ids() {
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        model_context_window: 120,
        reserved_output_tokens: 10,
        autocompact_buffer_tokens: 10,
        microcompact_trigger_ratio: 1.0,
        tool_result_compact_threshold: 20,
        tool_result_keep_last: 0,
        ..MemoryManagerConfig::default()
    });
    let mut assistant_call = Message::assistant("plan");
    assistant_call
        .tool_calls
        .push(ToolCall::new("call_1", "bash", BTreeMap::new()));
    let messages = vec![
        Message::system("sys"),
        Message::user("hello"),
        assistant_call,
        Message::tool("x".repeat(400), "call_1"),
    ];

    let (unchanged, changed) = manager.compact_for_cycle_with_usage(
        &messages,
        0,
        false,
        Some(100),
        Some(&BTreeSet::new()),
    );
    assert!(!changed);
    assert_eq!(unchanged, messages);

    let recent_tool_ids = BTreeSet::from(["call_1".to_string()]);
    let (compacted, changed) = manager.compact_for_cycle_with_usage(
        &messages,
        0,
        false,
        Some(100),
        Some(&recent_tool_ids),
    );

    assert!(changed);
    assert_eq!(compacted.len(), 4);
    assert!(compacted[3].content.contains("<Tool Result Compact>"));
    assert!(compacted
        .iter()
        .all(|message| !message.content.contains("<Compressed Agent Memory>")));
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
fn memory_manager_recomputes_length_after_tool_artifact_compaction() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        model_context_window: 160,
        reserved_output_tokens: 10,
        autocompact_buffer_tokens: 10,
        tool_result_compact_threshold: 20,
        tool_result_keep_last: 0,
        tool_result_excerpt_head: 1,
        tool_result_excerpt_tail: 1,
        workspace: Some(workspace.path().to_path_buf()),
        ..MemoryManagerConfig::default()
    });
    let mut assistant_call = Message::assistant("");
    assistant_call
        .tool_calls
        .push(ToolCall::new("call_1", "read_file", BTreeMap::new()));
    let messages = vec![
        Message::system("sys"),
        Message::user("read file"),
        assistant_call,
        Message::tool("x".repeat(400), "call_1"),
        Message::assistant("continue"),
    ];

    let (compacted, changed) =
        manager.compact_for_cycle_with_usage(&messages, 2, false, Some(500), None);

    assert!(changed);
    assert!(compacted.len() > 2);
    assert!(compacted
        .iter()
        .all(|message| !message.content.contains("<Compressed Agent Memory>")));
    assert!(compacted
        .iter()
        .any(|message| message.content.contains("<Tool Result Compact>")));
    assert!(workspace
        .path()
        .join(".memory/tool_results/cycle_2/call_1.txt")
        .exists());
    assert!(compacted.iter().any(|message| message
        .content
        .contains(".memory/tool_results/cycle_2/call_1.txt")));
}

#[test]
fn memory_manager_compacts_processed_image_payloads() {
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        model_context_window: 160,
        reserved_output_tokens: 10,
        autocompact_buffer_tokens: 10,
        ..MemoryManagerConfig::default()
    });
    let image_payload = format!("data:image/png;base64,{}", "a".repeat(400));
    let mut image_message = Message::user("[Image loaded] img.png");
    image_message.image_url = Some(image_payload);
    let messages = vec![
        Message::system("sys"),
        Message::user("original request"),
        image_message,
        Message::assistant("image parsed"),
        Message::assistant("next"),
    ];

    let (compacted, changed) =
        manager.compact_for_cycle_with_usage(&messages, 2, false, Some(500), None);

    assert!(changed);
    assert!(compacted.len() > 2);
    let compacted_image = compacted
        .iter()
        .find(|message| message.content.starts_with("[Image loaded]"))
        .expect("compacted image message");
    assert!(compacted_image.image_url.is_none());
    assert!(compacted_image.content.contains("image payload compacted"));
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
        microcompact_trigger_ratio: 0.5,
        ..MemoryManagerConfig::default()
    });

    assert_eq!(manager.effective_context_window(), 56_000);
    assert_eq!(manager.autocompact_threshold(), 50_000);
    assert_eq!(manager.warning_threshold(), 40_000);
    assert_eq!(manager.microcompact_trigger_threshold(), 25_000);
}

#[test]
fn memory_manager_persists_large_tool_results_as_artifacts() {
    let workspace = tempfile::tempdir().expect("workspace");
    let large_tool_result = "x".repeat(240);
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        compact_threshold: 10,
        model_context_window: 80,
        reserved_output_tokens: 10,
        autocompact_buffer_tokens: 0,
        tool_result_compact_threshold: 30,
        tool_result_keep_last: 0,
        tool_result_excerpt_head: 12,
        tool_result_excerpt_tail: 10,
        workspace: Some(workspace.path().to_path_buf()),
        ..MemoryManagerConfig::default()
    });
    let messages = vec![
        Message::system("system"),
        Message::user("read a large file"),
        Message {
            tool_calls: vec![ToolCall::new("call_1", "read_file", BTreeMap::new())],
            ..Message::assistant("reading")
        },
        Message::tool(large_tool_result.clone(), "call_1"),
        Message::assistant("continue"),
    ];

    let (compacted, changed) = manager.compact_for_cycle(&messages, 3, false);

    assert!(changed);
    let artifact = workspace
        .path()
        .join(".memory/tool_results/cycle_3/call_1.txt");
    assert!(
        artifact.is_file(),
        "missing artifact at {}",
        artifact.display()
    );
    assert_eq!(
        std::fs::read_to_string(&artifact).expect("artifact"),
        large_tool_result
    );
    assert!(compacted[1].content.contains("<Persisted Artifacts>"));
    assert!(compacted[1]
        .content
        .contains(".memory/tool_results/cycle_3/call_1.txt"));
    assert!(compacted[1].content.contains("tool: read_file"));
    assert!(compacted[1].content.contains("<Tool Result Compact>"));
    assert!(compacted[1]
        .content
        .contains("retrieval_hint: use read_file on artifact_path if needed"));
}

#[test]
fn memory_manager_does_not_persist_microcompacted_tool_results_as_artifacts() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        compact_threshold: 10,
        model_context_window: 80,
        reserved_output_tokens: 10,
        autocompact_buffer_tokens: 0,
        tool_result_compact_threshold: 1,
        tool_result_keep_last: 0,
        workspace: Some(workspace.path().to_path_buf()),
        ..MemoryManagerConfig::default()
    });
    let messages = vec![
        Message::system("system"),
        Message::user("continue from compacted output"),
        Message {
            tool_calls: vec![ToolCall::new("call_old", "read_file", BTreeMap::new())],
            ..Message::assistant("reading")
        },
        Message::tool(CLEARED_MARKER, "call_old"),
        Message::assistant("continue"),
    ];

    let (compacted, changed) = manager.compact(&messages, false);

    assert!(changed);
    assert!(!workspace
        .path()
        .join(".memory/tool_results/call_old.txt")
        .exists());
    assert!(compacted
        .iter()
        .all(|message| !message.content.contains("<Persisted Artifacts>")));
}

#[test]
fn memory_manager_restores_key_file_context_after_compaction() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("demo.py"), "print('restored')\n").expect("demo");
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        compact_threshold: 10,
        model_context_window: 80,
        reserved_output_tokens: 10,
        autocompact_buffer_tokens: 0,
        keep_recent_messages: 2,
        workspace: Some(workspace.path().to_path_buf()),
        ..MemoryManagerConfig::default()
    });
    let messages = vec![
        Message::system("sys"),
        Message::user("please update demo.py"),
        Message {
            tool_calls: vec![ToolCall::new(
                "call_1",
                "write_file",
                BTreeMap::from([
                    ("path".to_string(), json!("demo.py")),
                    ("content".to_string(), json!("print('restored')\n")),
                ]),
            )],
            ..Message::assistant("editing")
        },
        Message::tool("{\"ok\":true}", "call_1"),
        Message::assistant("waiting for verification"),
    ];

    let (compacted, changed) = manager.compact(&messages, true);

    assert!(changed);
    assert_eq!(compacted.len(), 2);
    assert!(compacted[1]
        .content
        .contains("\"files_examined_or_modified\":[{\"path\":\"demo.py\""));
    assert!(compacted[1]
        .content
        .contains("<Post-Compaction File Context>"));
    assert!(compacted[1].content.contains("path=\"demo.py\""));
    assert!(compacted[1].content.contains("print('restored')"));
}

#[test]
fn memory_manager_second_compaction_preserves_original_user_messages() {
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        compact_threshold: 10,
        model_context_window: 60,
        reserved_output_tokens: 10,
        autocompact_buffer_tokens: 0,
        keep_recent_messages: 2,
        ..MemoryManagerConfig::default()
    });
    let first_messages = vec![
        Message::system("sys"),
        Message::user("please preserve this exact request"),
        Message::assistant("working"),
    ];

    let (first_compacted, first_changed) = manager.compact(&first_messages, true);
    assert!(first_changed);

    let second_messages = vec![
        first_compacted[0].clone(),
        first_compacted[1].clone(),
        Message::assistant("made progress"),
        Message::user("and keep this follow-up too"),
    ];
    let (second_compacted, second_changed) = manager.compact(&second_messages, true);

    assert!(second_changed);
    assert!(second_compacted[1].content.contains(
        "\"original_user_messages\":[\"please preserve this exact request\",\"and keep this follow-up too\"]"
    ));
}

#[test]
fn memory_manager_uses_microcompact_before_full_summary() {
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        compact_threshold: 1_000,
        model_context_window: 4_000,
        reserved_output_tokens: 0,
        autocompact_buffer_tokens: 0,
        microcompact_trigger_ratio: 0.01,
        microcompact_keep_recent_cycles: 1,
        microcompact_min_result_length: 200,
        tool_result_compact_threshold: 2_000,
        ..MemoryManagerConfig::default()
    });
    let messages = vec![
        Message::system("sys"),
        Message::user("start"),
        Message {
            tool_calls: vec![ToolCall::new("call_old", "read_file", BTreeMap::new())],
            ..Message::assistant("old tool call")
        },
        Message::tool("x".repeat(600), "call_old"),
        Message::assistant("recent reply"),
        Message::user("latest ask"),
    ];

    let (compacted, changed) = manager.compact_for_cycle(&messages, 3, false);

    assert!(changed);
    assert!(compacted
        .iter()
        .any(|message| message.content == CLEARED_MARKER));
    assert!(compacted
        .iter()
        .all(|message| !message.content.contains("<Compressed Agent Memory>")));
}

#[test]
fn memory_manager_emergency_compact_preserves_recent_tool_context() {
    let manager = MemoryManager::new(MemoryManagerConfig {
        keep_recent_messages: 2,
        ..MemoryManagerConfig::default()
    });
    let messages = vec![
        Message::system("sys"),
        Message::user("old request"),
        Message {
            tool_calls: vec![ToolCall::new("call_1", "read_file", BTreeMap::new())],
            ..Message::assistant("call tool")
        },
        Message::tool("tool result", "call_1"),
        Message::assistant("recent analysis"),
        Message::user("latest ask"),
    ];

    let compacted = manager.emergency_compact(&messages, 0.5);

    assert_eq!(compacted[0].content, "sys");
    assert!(compacted
        .iter()
        .all(|message| message.content != "old request"));
    assert!(compacted
        .iter()
        .any(|message| message.role == vv_agent::MessageRole::Assistant
            && !message.tool_calls.is_empty()));
    assert!(compacted.iter().any(|message| {
        message.role == vv_agent::MessageRole::Tool
            && message.tool_call_id.as_deref() == Some("call_1")
    }));
}

#[test]
fn memory_manager_normalizes_orphan_tool_messages_with_reused_call_id() {
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        tool_calls_keep_last: 1,
        assistant_no_tool_keep_last: 10,
        ..MemoryManagerConfig::default()
    });
    let messages = vec![
        Message::system("sys"),
        Message {
            tool_calls: vec![ToolCall::new(
                "screen_capture:4",
                "screen_capture",
                BTreeMap::new(),
            )],
            ..Message::assistant("first capture request")
        },
        Message::tool("first result", "screen_capture:4"),
        Message::assistant("narration"),
        Message {
            tool_calls: vec![ToolCall::new(
                "screen_capture:4",
                "screen_capture",
                BTreeMap::new(),
            )],
            ..Message::assistant("second capture request")
        },
        Message::tool("second result", "screen_capture:4"),
    ];

    let (compacted, changed) = manager.compact(&messages, true);

    assert!(changed);
    assert!(!compacted[1].content.contains("first result"));
    assert!(compacted[1].content.contains("second result"));
    assert!(compacted[1].content.contains("second capture request"));
}

#[test]
fn memory_manager_drops_excess_tool_results_per_call_id() {
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        compact_threshold: 10,
        model_context_window: 80,
        reserved_output_tokens: 10,
        autocompact_buffer_tokens: 0,
        ..MemoryManagerConfig::default()
    });
    let messages = vec![
        Message::system("sys"),
        Message::user("run tool"),
        Message {
            tool_calls: vec![ToolCall::new("call_1", "bash", BTreeMap::new())],
            ..Message::assistant("call")
        },
        Message::tool("first", "call_1"),
        Message::tool("second", "call_1"),
        Message::assistant("done"),
    ];

    let (compacted, changed) = manager.compact(&messages, true);

    assert!(changed);
    assert!(compacted[1].content.contains("first"));
    assert!(!compacted[1].content.contains("second"));
}
