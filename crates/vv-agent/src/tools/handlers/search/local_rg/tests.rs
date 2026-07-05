use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::tools::base::ToolContext;

use super::{search_files_local_rg, RgSearchFilesRequest};

fn write_fake_rg(workspace: &Path, script: &str) -> PathBuf {
    let fake_rg = workspace.join("fake-rg");
    let mut file = std::fs::File::create(&fake_rg).expect("fake rg");
    file.write_all(script.as_bytes()).expect("fake rg body");
    file.sync_all().expect("fake rg sync");
    drop(file);
    let mut permissions = std::fs::metadata(&fake_rg).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_rg, permissions).expect("chmod");
    fake_rg
}

fn rg_request<'a>(
    context: &'a ToolContext,
    rg_executable: &'a Path,
    output_mode: &'a str,
) -> RgSearchFilesRequest<'a> {
    RgSearchFilesRequest {
        context,
        path: ".",
        glob_pattern: "**/*",
        pattern: "token",
        output_mode,
        file_type: Some("py"),
        case_insensitive: true,
        literal: false,
        multiline: false,
        before_context: 0,
        after_context: 0,
        include_hidden: false,
        include_ignored: false,
        include_sensitive: false,
        rg_executable,
    }
}

#[test]
fn search_files_rg_fast_path_parses_json_and_type_filter() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("a.py"), "token = 1\n").expect("a");
    std::fs::write(workspace.path().join("b.py"), "token = 2\n").expect("b");
    let fake_rg = write_fake_rg(
        workspace.path(),
        r#"#!/bin/sh
printf '%s\n' \
'{"type":"begin","data":{"path":{"text":"a.py"}}}' \
'{"type":"match","data":{"path":{"text":"a.py"},"lines":{"text":"token = 1\n"},"line_number":1,"submatches":[{"start":0,"end":5}]}}' \
'{"type":"end","data":{"path":{"text":"a.py"}}}' \
'{"type":"begin","data":{"path":{"text":"b.py"}}}' \
'{"type":"match","data":{"path":{"text":"b.py"},"lines":{"text":"token = 2\n"},"line_number":1,"submatches":[{"start":0,"end":5}]}}' \
'{"type":"end","data":{"path":{"text":"b.py"}}}' \
'{"type":"summary","data":{"stats":{"searches":7}}}'
"#,
    );

    let context = ToolContext::new(workspace.path());
    let result = search_files_local_rg(rg_request(&context, &fake_rg, "files_with_matches"))
        .expect("rg result");

    assert_eq!(result.files_with_matches, vec!["a.py", "b.py"]);
    assert_eq!(result.total_matches, 2);
    assert_eq!(result.files_searched, 7);
    assert_eq!(result.file_counts["a.py"], 1);
    assert_eq!(result.file_counts["b.py"], 1);
}

#[test]
fn search_files_rg_fast_path_accepts_returncode_2_with_results() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("a.py"), "no token here\n").expect("a");
    let fake_rg = write_fake_rg(
        workspace.path(),
        r#"#!/bin/sh
printf '%s\n' \
'{"type":"begin","data":{"path":{"text":"a.py"}}}' \
'{"type":"match","data":{"path":{"text":"a.py"},"lines":{"text":"Agent from rg\n"},"line_number":1,"submatches":[{"start":0,"end":5}]}}' \
'{"type":"summary","data":{}}'
exit 2
"#,
    );

    let context = ToolContext::new(workspace.path());
    let mut request = rg_request(&context, &fake_rg, "content");
    request.pattern = "Agent";
    let result = search_files_local_rg(request).expect("rg result");

    assert_eq!(result.total_matches, 1);
    assert_eq!(result.rows[0]["path"], "a.py");
    assert_eq!(result.rows[0]["text"], "Agent from rg");
}

#[test]
fn search_files_rg_fast_path_returns_none_on_hard_error() {
    let workspace = tempfile::tempdir().expect("workspace");
    let fake_rg = write_fake_rg(workspace.path(), "#!/bin/sh\nexit 3\n");

    let context = ToolContext::new(workspace.path());
    let result = search_files_local_rg(rg_request(&context, &fake_rg, "content"));

    assert!(result.is_none());
}
