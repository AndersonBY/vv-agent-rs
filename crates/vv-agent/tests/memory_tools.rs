use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use vv_agent::{
    build_default_registry, memory::token_utils::compute_compaction_threshold, MemoryManager,
    MemoryManagerConfig, Message, SessionMemory, SessionMemoryConfig, SessionMemoryEntry, ToolCall,
    ToolContext, ToolResultStatus,
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

    let (compacted, changed) = manager.compact(&messages, false);

    assert!(changed);
    let artifact = workspace.path().join(".memory/tool_results/call_1.txt");
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
        .contains(".memory/tool_results/call_1.txt"));
    assert!(compacted[1].content.contains("tool: read_file"));
    assert!(compacted[1].content.contains("<Tool Result Compact>"));
    assert!(compacted[1]
        .content
        .contains("retrieval_hint: use read_file on artifact_path if needed"));
}

#[test]
fn session_memory_extracts_new_messages_and_renders_grouped_context() {
    let prompts = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured_prompts = Arc::clone(&prompts);
    let mut memory = SessionMemory::new(SessionMemoryConfig {
        min_tokens_before_extraction: 50,
        min_text_messages: 1,
        extraction_callback: Some(Arc::new(move |prompt, _backend, _model| {
            captured_prompts
                .lock()
                .expect("prompts")
                .push(prompt.to_string());
            if prompt.contains("gamma") {
                Some(
                    r#"[{"category":"file_change","content":"updated manager.rs","importance":7}]"#
                        .to_string(),
                )
            } else {
                Some(
                    r#"[{"category":"decision","content":"keep tests green","importance":8}]"#
                        .to_string(),
                )
            }
        })),
        ..SessionMemoryConfig::default()
    });
    assert_eq!(memory.state.last_extracted_message_index, -1);
    let messages = vec![
        Message::system("sys"),
        Message::user("alpha"),
        Message::assistant("beta"),
    ];

    assert!(memory.should_extract(50, 1));
    assert_eq!(memory.extract(&messages, 4, 80), 1);

    let updated_messages = [messages, vec![Message::user("gamma")]].concat();
    assert_eq!(memory.extract(&updated_messages, 5, 140), 1);

    let prompts = prompts.lock().expect("prompts");
    assert!(prompts[0].contains("alpha"));
    assert!(prompts[0].contains("beta"));
    assert!(prompts[1].contains("gamma"));
    assert!(!prompts[1].contains("alpha"));
    drop(prompts);

    let rendered = memory.render_as_system_context();
    assert!(rendered.starts_with("<Session Memory>"));
    assert!(rendered.contains("## decision"));
    assert!(rendered.contains("- keep tests green"));
    assert!(rendered.contains("## file_change"));
    assert!(rendered.ends_with("</Session Memory>"));
}

#[test]
fn session_memory_persists_scoped_state_and_rejects_path_traversal() {
    let workspace = tempfile::tempdir().expect("workspace");
    let mut memory = SessionMemory::with_workspace(
        SessionMemoryConfig {
            storage_dir: ".memory/session".into(),
            ..SessionMemoryConfig::default()
        },
        Some(workspace.path().to_path_buf()),
        Some("task-a".to_string()),
    );
    memory.state.entries = vec![SessionMemoryEntry::new(
        "user_intent",
        "finish phase 4",
        9,
        10,
    )];
    memory.state.last_extracted_message_index = 12;
    memory.state.tokens_at_last_extraction = 320;
    memory.state.initialized = true;
    memory.save();

    let mut loaded = SessionMemory::with_workspace(
        SessionMemoryConfig {
            storage_dir: ".memory/session".into(),
            ..SessionMemoryConfig::default()
        },
        Some(workspace.path().to_path_buf()),
        Some("task-a".to_string()),
    );
    loaded.load();

    assert_eq!(loaded.state.entries.len(), 1);
    assert_eq!(loaded.state.entries[0].content, "finish phase 4");
    assert_eq!(loaded.state.last_extracted_message_index, 12);

    loaded.on_compaction(Some(33));
    assert_eq!(loaded.state.last_extracted_message_index, -1);
    assert_eq!(loaded.state.tokens_at_last_extraction, 33);
    assert_eq!(loaded.state.entries[0].content, "finish phase 4");

    let mut isolated = SessionMemory::with_workspace(
        SessionMemoryConfig {
            storage_dir: ".memory/session".into(),
            ..SessionMemoryConfig::default()
        },
        Some(workspace.path().to_path_buf()),
        Some("task-b".to_string()),
    );
    isolated.load();
    assert!(isolated.state.entries.is_empty());

    let escaping = SessionMemory::with_workspace(
        SessionMemoryConfig {
            storage_dir: "../../outside".into(),
            ..SessionMemoryConfig::default()
        },
        Some(workspace.path().to_path_buf()),
        None,
    );
    assert!(escaping.storage_path().is_none());
}

#[test]
fn session_memory_normalizes_dedupes_and_prunes_low_importance_entries() {
    let mut memory = SessionMemory::new(SessionMemoryConfig {
        min_tokens_before_extraction: 100,
        min_text_messages: 5,
        max_tokens: 80,
        token_model: "demo".to_string(),
        ..SessionMemoryConfig::default()
    });

    assert!(!memory.should_extract(99, 5));
    assert!(!memory.should_extract(10_000, 4));
    memory.config.extraction_callback = Some(Arc::new(|_, _, _| Some("[]".to_string())));
    assert!(memory.should_extract(10_000, 5));
    memory.state.initialized = true;
    memory.state.tokens_at_last_extraction = 120;
    assert!(!memory.should_extract(169, 5));
    assert!(memory.should_extract(170, 5));
    memory.state.tokens_at_last_extraction = 500;
    assert!(!memory.should_extract(40, 5));
    assert!(memory.should_extract(120, 5));

    memory.state.entries = vec![
        SessionMemoryEntry::new("unknown", "a".repeat(180), 1, 9),
        SessionMemoryEntry::new("key_fact", "b".repeat(180), 2, 2),
        SessionMemoryEntry::new("key_fact", "c".repeat(180), 3, 5),
    ];
    memory.merge_entries(vec![SessionMemoryEntry::new(
        "KEY_FACT",
        format!("  {}  ", "a".repeat(180)),
        7,
        10,
    )]);
    memory.prune_to_budget();

    let remaining = memory
        .state
        .entries
        .iter()
        .map(|entry| entry.content.as_str())
        .collect::<Vec<_>>();
    assert!(remaining.contains(&"a".repeat(180).as_str()));
    assert!(!remaining.contains(&"b".repeat(180).as_str()));
    assert_eq!(memory.state.entries[0].category, "key_fact");
    assert_eq!(memory.state.entries[0].importance, 10);
    assert_eq!(memory.state.entries[0].source_cycle, 7);
}

#[test]
fn memory_manager_preserves_session_memory_across_compaction() {
    let prompts = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured_prompts = Arc::clone(&prompts);
    let session_memory = SessionMemory::new(SessionMemoryConfig {
        min_tokens_before_extraction: 20,
        min_text_messages: 2,
        extraction_callback: Some(Arc::new(move |prompt, _backend, _model| {
            captured_prompts
                .lock()
                .expect("prompts")
                .push(prompt.to_string());
            Some(
                r#"[{"category":"key_fact","content":"preserve prior decisions","importance":9}]"#
                    .to_string(),
            )
        })),
        token_model: "demo".to_string(),
        ..SessionMemoryConfig::default()
    });
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        compact_threshold: 10,
        model_context_window: 70,
        reserved_output_tokens: 10,
        autocompact_buffer_tokens: 0,
        keep_recent_messages: 2,
        model: "demo".to_string(),
        session_memory: Some(session_memory),
        ..MemoryManagerConfig::default()
    });
    let messages = vec![
        Message::system("sys"),
        Message::user("u".repeat(40)),
        Message::assistant("a".repeat(40)),
        Message::user("c".repeat(40)),
    ];

    let (compacted, changed) = manager.compact(&messages, false);

    assert!(changed);
    assert_eq!(compacted.len(), 2);
    let session_memory = manager.session_memory().expect("session memory");
    assert!(!session_memory.state.entries.is_empty());
    assert_eq!(session_memory.state.last_extracted_message_index, -1);
    let prompts = prompts.lock().expect("prompts");
    assert!(!prompts[0].contains("<Session Memory>"));
    drop(prompts);

    let request_messages = manager.apply_session_memory_context(&compacted);
    assert!(request_messages[0].content.contains("<Session Memory>"));
    assert!(request_messages[0]
        .content
        .contains("preserve prior decisions"));
}
