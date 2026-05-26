use serde_json::json;
use vv_agent::memory::{restore_key_files, PostCompactRestoreConfig};

#[test]
fn restore_key_files_prioritizes_modified_then_created() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("a.py"), "print('a')\n").expect("a");
    std::fs::write(workspace.path().join("b.py"), "print('b')\n").expect("b");
    std::fs::write(workspace.path().join("c.py"), "print('c')\n").expect("c");

    let restored = restore_key_files(
        &json!({
            "files_examined_or_modified": [
                {"path": "c.py", "action": "read", "summary": "read c"},
                {"path": "a.py", "action": "modified", "summary": "updated a"},
                {"path": "b.py", "action": "created", "summary": "created b"},
            ]
        }),
        Some(workspace.path()),
        &PostCompactRestoreConfig {
            max_files: 2,
            ..PostCompactRestoreConfig::default()
        },
    );

    assert!(restored.find("path=\"a.py\"") < restored.find("path=\"b.py\""));
    assert!(!restored.contains("path=\"c.py\""));
}

#[test]
fn restore_key_files_respects_single_file_budget() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("big.py"), "x = 1\n".repeat(400)).expect("big");

    let restored = restore_key_files(
        &json!({
            "files_examined_or_modified": [
                {"path": "big.py", "action": "modified", "summary": "updated big"},
            ]
        }),
        Some(workspace.path()),
        &PostCompactRestoreConfig {
            max_tokens_per_file: 40,
            total_budget_tokens: 200,
            ..PostCompactRestoreConfig::default()
        },
    );

    assert!(restored.contains("<Post-Compaction File Context>"));
    assert!(restored.contains("truncated after compaction restore"));
}

#[test]
fn restore_key_files_respects_total_budget() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("a.py"), "a = 1\n".repeat(200)).expect("a");
    std::fs::write(workspace.path().join("b.py"), "b = 2\n".repeat(200)).expect("b");

    let restored = restore_key_files(
        &json!({
            "files_examined_or_modified": [
                {"path": "a.py", "action": "modified", "summary": "updated a"},
                {"path": "b.py", "action": "created", "summary": "created b"},
            ]
        }),
        Some(workspace.path()),
        &PostCompactRestoreConfig {
            total_budget_tokens: 180,
            max_tokens_per_file: 120,
            ..PostCompactRestoreConfig::default()
        },
    );

    assert!(restored.contains("path=\"a.py\""));
    assert!(!restored.contains("path=\"b.py\""));
}

#[test]
fn restore_key_files_skips_missing_and_escaped_paths() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("safe.py"), "print('safe')\n").expect("safe");

    let restored = restore_key_files(
        &json!({
            "files_examined_or_modified": [
                {"path": "../../etc/passwd", "action": "modified", "summary": "bad"},
                {"path": "missing.py", "action": "read", "summary": "missing"},
                {"path": "safe.py", "action": "read", "summary": "safe"},
            ]
        }),
        Some(workspace.path()),
        &PostCompactRestoreConfig::default(),
    );

    assert!(!restored.contains("../../etc/passwd"));
    assert!(!restored.contains("missing.py"));
    assert!(restored.contains("path=\"safe.py\""));
}
