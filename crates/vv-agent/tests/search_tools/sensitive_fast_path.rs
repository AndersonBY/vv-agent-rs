use std::collections::BTreeMap;
use std::sync::Mutex;

use serde_json::json;
use vv_agent::{build_default_registry, ToolCall, ToolContext};

static PATH_LOCK: Mutex<()> = Mutex::new(());

fn find_real_rg() -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|directory| directory.join("rg"))
            .find(|candidate| candidate.is_file())
    })
}

fn shell_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

fn write_fake_rg(directory: &std::path::Path, real_rg: &std::path::Path, script: &str) {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;

    let path = directory.join("rg");
    let mut file = std::fs::File::create(&path).expect("fake rg");
    let body = format!(
        "#!/bin/sh\n\
         if [ -e .force_fake_rg ]; then\n\
         {script}\n\
         else\n\
         exec {} \"$@\"\n\
         fi\n",
        shell_quote(real_rg)
    );
    file.write_all(body.as_bytes()).expect("fake rg body");
    file.sync_all().expect("fake rg sync");
    drop(file);
    let mut permissions = std::fs::metadata(&path)
        .expect("fake rg metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("fake rg chmod");
}

fn with_fake_rg_path<T>(script: &str, run: impl FnOnce() -> T) -> T {
    let _guard = PATH_LOCK.lock().expect("path lock");
    let bin = tempfile::tempdir().expect("fake rg dir");
    let bin_path = bin.path().to_path_buf();
    let real_rg = find_real_rg().expect("real rg executable");
    write_fake_rg(&bin_path, &real_rg, script);
    let original_path = std::env::var_os("PATH");
    let fake_path = match &original_path {
        Some(path) => std::env::join_paths(
            std::iter::once(bin_path.clone()).chain(std::env::split_paths(path)),
        )
        .expect("join fake PATH"),
        None => bin_path.clone().into_os_string(),
    };
    std::env::set_var("PATH", fake_path);
    let result = run();
    match original_path {
        Some(path) => std::env::set_var("PATH", path),
        None => std::env::remove_var("PATH"),
    }
    std::mem::forget(bin);
    result
}

#[test]
fn search_files_default_sensitive_filter_uses_rg_fast_path_when_no_sensitive_candidates() {
    with_fake_rg_path(
        r#"#!/bin/sh
printf '%s\n' \
'{"type":"begin","data":{"path":{"text":"visible.txt"}}}' \
'{"type":"match","data":{"path":{"text":"visible.txt"},"lines":{"text":"TOKEN from fake rg\n"},"line_number":1,"submatches":[{"start":0,"end":5}]}}' \
'{"type":"summary","data":{"stats":{"searches":1}}}'
"#,
        || {
            let workspace = tempfile::tempdir().expect("workspace");
            let registry = build_default_registry();
            let mut context = ToolContext::new(workspace.path());
            std::fs::create_dir(workspace.path().join(".force_fake_rg")).expect("marker");
            std::fs::write(workspace.path().join("visible.txt"), "no match").expect("visible");

            let result = registry
                .execute(
                    &ToolCall::new(
                        "search_default_rg_fast_path",
                        "search_files",
                        BTreeMap::from([("pattern".to_string(), json!("TOKEN"))]),
                    ),
                    &mut context,
                )
                .expect("search token");

            assert_eq!(result.metadata["files"], json!(["visible.txt"]));
            assert_eq!(result.metadata["sensitive_files_omitted"], 0);
            assert_eq!(result.metadata["summary"]["files_searched"], 1);
        },
    );
}

#[test]
fn search_files_default_hidden_sensitive_path_still_uses_rg_fast_path() {
    with_fake_rg_path(
        r#"#!/bin/sh
printf '%s\n' \
'{"type":"begin","data":{"path":{"text":"visible.txt"}}}' \
'{"type":"match","data":{"path":{"text":"visible.txt"},"lines":{"text":"TOKEN from fake rg\n"},"line_number":1,"submatches":[{"start":0,"end":5}]}}' \
'{"type":"summary","data":{"stats":{"searches":1}}}'
"#,
        || {
            let workspace = tempfile::tempdir().expect("workspace");
            let registry = build_default_registry();
            let mut context = ToolContext::new(workspace.path());
            std::fs::create_dir(workspace.path().join(".force_fake_rg")).expect("marker");
            std::fs::create_dir_all(workspace.path().join(".config")).expect("config dir");
            std::fs::write(
                workspace.path().join(".config/service_token.json"),
                r#"{"token":"TOKEN"}"#,
            )
            .expect("token");
            std::fs::write(workspace.path().join("visible.txt"), "no match").expect("visible");

            let result = registry
                .execute(
                    &ToolCall::new(
                        "search_hidden_sensitive_rg_fast_path",
                        "search_files",
                        BTreeMap::from([("pattern".to_string(), json!("TOKEN"))]),
                    ),
                    &mut context,
                )
                .expect("search token");

            assert_eq!(result.metadata["files"], json!(["visible.txt"]));
            assert_eq!(result.metadata["sensitive_files_omitted"], 1);
            assert_eq!(result.metadata["summary"]["files_searched"], 1);
        },
    );
}

#[test]
fn search_files_default_ignored_sensitive_path_still_uses_rg_fast_path() {
    with_fake_rg_path(
        r#"#!/bin/sh
printf '%s\n' \
'{"type":"begin","data":{"path":{"text":"visible.txt"}}}' \
'{"type":"match","data":{"path":{"text":"visible.txt"},"lines":{"text":"TOKEN from fake rg\n"},"line_number":1,"submatches":[{"start":0,"end":5}]}}' \
'{"type":"summary","data":{"stats":{"searches":1}}}'
"#,
        || {
            let workspace = tempfile::tempdir().expect("workspace");
            let registry = build_default_registry();
            let mut context = ToolContext::new(workspace.path());
            std::fs::create_dir(workspace.path().join(".force_fake_rg")).expect("marker");
            std::fs::create_dir_all(workspace.path().join("node_modules/.config"))
                .expect("ignored config dir");
            std::fs::write(
                workspace
                    .path()
                    .join("node_modules/.config/service_token.json"),
                r#"{"token":"TOKEN"}"#,
            )
            .expect("token");
            std::fs::write(workspace.path().join("visible.txt"), "no match").expect("visible");

            let result = registry
                .execute(
                    &ToolCall::new(
                        "search_ignored_sensitive_rg_fast_path",
                        "search_files",
                        BTreeMap::from([
                            ("pattern".to_string(), json!("TOKEN")),
                            ("include_hidden".to_string(), json!(true)),
                        ]),
                    ),
                    &mut context,
                )
                .expect("search token");

            assert_eq!(result.metadata["files"], json!(["visible.txt"]));
            assert_eq!(result.metadata["sensitive_files_omitted"], 1);
            assert_eq!(result.metadata["summary"]["files_searched"], 1);
        },
    );
}

#[test]
fn search_files_default_sensitive_filter_preserves_rg_fast_path_when_excludes_cover_paths() {
    with_fake_rg_path(
        r#"#!/bin/sh
has_pem_exclude=0
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--glob" ] && [ "${2:-}" = "!**/*.pem" ]; then
    has_pem_exclude=1
  fi
  shift
done
if [ "$has_pem_exclude" -ne 1 ]; then
  echo "missing pem sensitive exclude" >&2
  exit 3
fi
printf '%s\n' \
'{"type":"begin","data":{"path":{"text":"visible.txt"}}}' \
'{"type":"match","data":{"path":{"text":"visible.txt"},"lines":{"text":"TOKEN from fake rg\n"},"line_number":1,"submatches":[{"start":0,"end":5}]}}' \
'{"type":"summary","data":{"stats":{"searches":1}}}'
"#,
        || {
            let workspace = tempfile::tempdir().expect("workspace");
            let registry = build_default_registry();
            let mut context = ToolContext::new(workspace.path());
            std::fs::create_dir(workspace.path().join(".force_fake_rg")).expect("marker");
            std::fs::write(workspace.path().join("private.pem"), "TOKEN=secret").expect("pem");
            std::fs::write(workspace.path().join("visible.txt"), "no match").expect("visible");

            let result = registry
                .execute(
                    &ToolCall::new(
                        "search_sensitive_rg_fast_path",
                        "search_files",
                        BTreeMap::from([("pattern".to_string(), json!("TOKEN"))]),
                    ),
                    &mut context,
                )
                .expect("search token");

            assert_eq!(result.metadata["files"], json!(["visible.txt"]));
            assert_eq!(result.metadata["sensitive_files_omitted"], 1);
            assert_eq!(result.metadata["summary"]["files_searched"], 1);
        },
    );
}

#[test]
fn search_files_does_not_invoke_rg_when_config_token_path_is_sensitive() {
    with_fake_rg_path(
        r#"#!/bin/sh
printf '%s\n' \
'{"type":"begin","data":{"path":{"text":"visible.txt"}}}' \
'{"type":"match","data":{"path":{"text":"visible.txt"},"lines":{"text":"TOKEN from fake rg\n"},"line_number":1,"submatches":[{"start":0,"end":5}]}}' \
'{"type":"summary","data":{"stats":{"searches":1}}}'
"#,
        || {
            let workspace = tempfile::tempdir().expect("workspace");
            let registry = build_default_registry();
            let mut context = ToolContext::new(workspace.path());
            std::fs::create_dir(workspace.path().join(".force_fake_rg")).expect("marker");
            std::fs::create_dir_all(workspace.path().join(".config")).expect("config dir");
            std::fs::write(
                workspace.path().join(".config/service_token.json"),
                r#"{"token":"TOKEN"}"#,
            )
            .expect("token");
            std::fs::write(workspace.path().join("visible.txt"), "no match").expect("visible");

            let result = registry
                .execute(
                    &ToolCall::new(
                        "search_config_token",
                        "search_files",
                        BTreeMap::from([
                            ("pattern".to_string(), json!("TOKEN")),
                            ("include_hidden".to_string(), json!(true)),
                        ]),
                    ),
                    &mut context,
                )
                .expect("search token");

            assert_eq!(result.metadata["files"], json!([]));
            assert_eq!(result.metadata["sensitive_files_omitted"], 1);
            assert_eq!(result.metadata["summary"]["files_searched"], 1);
        },
    );
}

#[test]
fn search_files_does_not_treat_uppercase_p8_as_rg_exclude_covered() {
    with_fake_rg_path(
        r#"#!/bin/sh
printf '%s\n' \
'{"type":"begin","data":{"path":{"text":"visible.txt"}}}' \
'{"type":"match","data":{"path":{"text":"visible.txt"},"lines":{"text":"PRIVATE from fake rg\n"},"line_number":1,"submatches":[{"start":0,"end":7}]}}' \
'{"type":"summary","data":{"stats":{"searches":1}}}'
"#,
        || {
            let workspace = tempfile::tempdir().expect("workspace");
            let registry = build_default_registry();
            let mut context = ToolContext::new(workspace.path());
            std::fs::create_dir(workspace.path().join(".force_fake_rg")).expect("marker");
            std::fs::create_dir_all(workspace.path().join("keys")).expect("keys dir");
            std::fs::write(workspace.path().join("keys/AuthKey_ABC123.P8"), "PRIVATE").expect("p8");
            std::fs::write(workspace.path().join("visible.txt"), "no match").expect("visible");

            let result = registry
                .execute(
                    &ToolCall::new(
                        "search_upper_p8",
                        "search_files",
                        BTreeMap::from([("pattern".to_string(), json!("PRIVATE"))]),
                    ),
                    &mut context,
                )
                .expect("search p8");

            assert_eq!(result.metadata["files"], json!([]));
            assert_eq!(result.metadata["sensitive_files_omitted"], 1);
            assert_eq!(result.metadata["summary"]["files_searched"], 1);
        },
    );
}
