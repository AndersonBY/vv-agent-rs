use super::*;

fn fallback_name_matches(name: &str) -> bool {
    let Some(hex) = name
        .strip_prefix("tool_result_")
        .and_then(|name| name.strip_suffix(".txt"))
    else {
        return false;
    };
    hex.len() == 32 && hex.chars().all(|character| character.is_ascii_hexdigit())
}

fn artifact_messages(tool_call_id: &str, contents: &[&str]) -> Vec<Message> {
    let calls = contents
        .iter()
        .map(|_| ToolCall::new(tool_call_id, "read_file", BTreeMap::new()))
        .collect::<Vec<_>>();
    let mut messages = vec![
        Message::system("system"),
        Message::user("read files"),
        Message {
            tool_calls: calls,
            ..Message::assistant("reading")
        },
    ];
    messages.extend(
        contents
            .iter()
            .map(|content| Message::tool(*content, tool_call_id)),
    );
    messages.push(Message::assistant("continue"));
    messages
}

fn compact_artifacts(
    workspace: &Path,
    artifact_dir: &str,
    tool_call_id: &str,
    contents: &[&str],
) -> Vec<Message> {
    let mut manager = MemoryManager::new(MemoryManagerConfig {
        compact_threshold: 10,
        model_context_window: 80,
        reserved_output_tokens: 10,
        autocompact_buffer_tokens: 0,
        tool_result_compact_threshold: 10,
        tool_result_keep_last: 0,
        tool_result_artifact_dir: artifact_dir.into(),
        workspace: Some(workspace.to_path_buf()),
        ..MemoryManagerConfig::default()
    });
    manager
        .compact_for_cycle(&artifact_messages(tool_call_id, contents), 4, false)
        .0
}

#[test]
fn artifact_fallbacks_are_unique_and_fail_open_at_workspace_boundary() {
    let contract = contract();
    let expected = &contract["artifacts"];
    assert_eq!(
        expected["fallback_pattern"].as_str().unwrap(),
        "^tool_result_[0-9a-f]{32}\\.txt$"
    );
    let root = tempfile::tempdir().expect("test root");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace");
    let compacted = compact_artifacts(
        &workspace,
        ".memory/tool_results",
        "/",
        &["first artifact payload", "second artifact payload"],
    );
    assert!(compacted
        .iter()
        .any(|message| message.content.contains("<Persisted Artifacts>")));
    let artifact_dir = workspace.join(".memory/tool_results/cycle_4");
    let mut fallback_names = std::fs::read_dir(&artifact_dir)
        .expect("artifact directory")
        .map(|entry| {
            entry
                .expect("artifact entry")
                .file_name()
                .to_string_lossy()
                .to_string()
        })
        .collect::<Vec<_>>();
    fallback_names.sort();
    assert_eq!(
        fallback_names.len() as u64,
        expected["fallback_count"].as_u64().unwrap()
    );
    assert!(fallback_names
        .iter()
        .all(|name| fallback_name_matches(name)));
    assert_ne!(fallback_names[0], fallback_names[1]);

    let blocked = workspace.join("blocked");
    std::fs::write(&blocked, "not a directory").expect("blocked path");
    let failed = compact_artifacts(
        &workspace,
        "blocked/nested",
        "call",
        &["write failure payload"],
    );
    assert!(failed
        .iter()
        .any(|message| message.content.contains("artifact_path: N/A")));
    assert_eq!(expected["write_failure_path"].as_str().unwrap(), "N/A");

    let escaped = compact_artifacts(&workspace, "../outside", "call", &["escape payload"]);
    assert!(escaped
        .iter()
        .any(|message| message.content.contains("artifact_path: N/A")));
    assert_eq!(expected["escape_path"].as_str().unwrap(), "N/A");
    assert!(!root.path().join("outside").exists());
}
