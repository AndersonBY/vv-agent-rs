use super::*;

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
fn session_memory_skips_compacted_summary_messages_during_extraction() {
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
            Some(
                r#"[{"category":"key_fact","content":"new follow-up","importance":6}]"#.to_string(),
            )
        })),
        ..SessionMemoryConfig::default()
    });
    let messages = vec![
        Message::system("sys"),
        Message::user(
            "<Original User Request>\nstart\n</Original User Request>\n\n\
<Compressed Agent Memory>\nsummary\n</Compressed Agent Memory>",
        ),
        Message::assistant("new answer"),
    ];

    let merged = memory.extract(&messages, 7, 120);

    assert_eq!(merged, 1);
    let prompts = prompts.lock().expect("prompts");
    assert!(!prompts[0].contains("<Compressed Agent Memory>"));
    assert!(!prompts[0].contains("summary"));
    assert!(prompts[0].contains("new answer"));
}

#[test]
fn session_memory_extract_handles_callback_panic() {
    let mut memory = SessionMemory::new(SessionMemoryConfig {
        min_tokens_before_extraction: 50,
        min_text_messages: 1,
        extraction_callback: Some(Arc::new(|_, _, _| panic!("boom"))),
        ..SessionMemoryConfig::default()
    });

    let merged = memory.extract(&[Message::system("sys"), Message::user("alpha")], 1, 80);

    assert_eq!(merged, 0);
    assert!(memory.state.entries.is_empty());
    assert!(!memory.state.initialized);
}

#[test]
fn session_memory_parse_handles_non_array_and_greedy_noise() {
    let memory = SessionMemory::new(SessionMemoryConfig::default());

    assert!(memory
        .parse_extraction_result(r#"{"category":"key_fact"}"#, 1)
        .is_empty());

    let parsed = memory.parse_extraction_result(
        r#"prefix [{"category":"key_fact","content":"ok","importance":6}] trailing ] noise"#,
        2,
    );

    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].content, "ok");
    assert_eq!(parsed[0].source_cycle, 2);
    assert_eq!(parsed[0].importance, 6);
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

#[test]
fn memory_manager_compact_directly_applies_session_memory_context() {
    let mut session_memory = SessionMemory::new(SessionMemoryConfig {
        token_model: "demo".to_string(),
        ..SessionMemoryConfig::default()
    });
    session_memory.state.entries = vec![SessionMemoryEntry::new(
        "decision",
        "keep the Rust API small",
        2,
        9,
    )];
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        compact_threshold: 10_000,
        model_context_window: 20_000,
        reserved_output_tokens: 100,
        autocompact_buffer_tokens: 0,
        model: "demo".to_string(),
        session_memory: Some(session_memory),
        ..MemoryManagerConfig::default()
    });
    let messages = vec![Message::system("sys"), Message::user("small")];

    let (updated, changed) = manager.compact(&messages, false);

    assert!(!changed);
    assert_eq!(updated.len(), 2);
    assert!(updated[0].content.starts_with("sys"));
    assert!(updated[0].content.contains("<Session Memory>"));
    assert!(updated[0].content.contains("keep the Rust API small"));
}

#[test]
fn memory_manager_extracts_session_memory_before_returning_small_requests() {
    let calls = Arc::new(Mutex::new(0usize));
    let calls_for_callback = Arc::clone(&calls);
    let session_memory = SessionMemory::new(SessionMemoryConfig {
        min_tokens_before_extraction: 1,
        min_text_messages: 1,
        extraction_callback: Some(Arc::new(move |_, _, _| {
            *calls_for_callback.lock().expect("calls poisoned") += 1;
            Some(
                json!([{
                    "category": "key_fact",
                    "content": "small request fact",
                    "importance": 9
                }])
                .to_string(),
            )
        })),
        token_model: "demo".to_string(),
        ..SessionMemoryConfig::default()
    });
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        compact_threshold: 10_000,
        model_context_window: 10_000,
        reserved_output_tokens: 0,
        autocompact_buffer_tokens: 0,
        model: "demo".to_string(),
        session_memory: Some(session_memory),
        ..MemoryManagerConfig::default()
    });

    let (messages, _changed) = manager.compact_for_cycle_with_usage(
        &[Message::system("system"), Message::user("remember alpha")],
        2,
        false,
        None,
        None,
    );

    assert_eq!(*calls.lock().expect("calls poisoned"), 1);
    assert!(
        messages
            .first()
            .is_some_and(|message| message.content.contains("<Session Memory>")
                && message.content.contains("small request fact")),
        "session memory should be extracted and injected before returning: {messages:#?}"
    );
}
