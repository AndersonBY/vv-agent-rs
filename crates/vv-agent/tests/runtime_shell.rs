use serde_json::json;
use vv_agent::runtime::shell::{
    build_shell_invocation, normalize_windows_shell_priority, prepare_shell_execution,
    resolve_shell_invocation,
};

#[test]
fn runtime_shell_resolution_matches_python_posix_defaults() {
    let resolved = resolve_shell_invocation(None, None).expect("default shell");
    assert_eq!(resolved.kind, "bash");
    assert_eq!(resolved.name, "bash");
    assert_eq!(resolved.prefix, vec!["bash", "-lc"]);

    let auto = resolve_shell_invocation(Some("auto"), None).expect("auto shell");
    assert_eq!(auto.prefix, vec!["bash", "-lc"]);
}

#[test]
fn runtime_shell_prepares_auto_confirm_like_python() {
    let prepared = prepare_shell_execution("cargo test", true, Some("tail"), Some("bash"), None)
        .expect("prepared bash");
    assert_eq!(prepared.command, vec!["bash", "-lc", "yes | (cargo test)"]);
    assert_eq!(prepared.shell.as_deref(), Some("bash"));
    assert_eq!(prepared.stdin.as_deref(), Some("tail"));

    let powershell = prepare_shell_execution(
        "Install-Module Demo",
        true,
        Some("after"),
        Some("powershell"),
        None,
    )
    .expect("prepared powershell");
    assert_eq!(powershell.kind, "powershell");
    assert!(powershell
        .stdin
        .as_deref()
        .expect("auto confirm stdin")
        .starts_with("y\ny\n"));
    assert!(powershell
        .stdin
        .as_deref()
        .expect("auto confirm stdin")
        .ends_with("after"));
}

#[test]
fn runtime_shell_builds_invocation_helper_like_python() {
    let invocation =
        build_shell_invocation("echo hello", Some("bash"), None).expect("shell invocation");

    assert_eq!(invocation, vec!["bash", "-lc", "echo hello"]);
}

#[test]
fn runtime_shell_normalizes_metadata_priority_with_python_str_truthiness() {
    let priority = normalize_windows_shell_priority(Some(&json!([
        "git-bash", 7, true, false, null, "", 0.0, "git-bash"
    ])))
    .expect("priority");

    assert_eq!(
        priority,
        Some(vec![
            "git-bash".to_string(),
            "7".to_string(),
            "True".to_string(),
        ])
    );
}
