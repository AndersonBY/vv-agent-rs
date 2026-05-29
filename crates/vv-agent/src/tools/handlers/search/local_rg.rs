use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine as _;
use serde_json::{json, Value};

use crate::tools::base::ToolContext;
use crate::tools::common::{
    command_output_with_executable_busy_retry, is_ignored_root, matches_file_type,
    workspace_relative_path_or_absolute,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RgGrepResult {
    pub files_searched: usize,
    pub total_matches: usize,
    pub files_with_matches: Vec<String>,
    pub file_counts: BTreeMap<String, usize>,
    pub rows: Vec<Value>,
}

pub(super) struct RgWorkspaceGrepRequest<'a> {
    pub context: &'a ToolContext,
    pub path: &'a str,
    pub glob_pattern: &'a str,
    pub pattern: &'a str,
    pub output_mode: &'a str,
    pub file_type: Option<&'a str>,
    pub case_insensitive: bool,
    pub multiline: bool,
    pub before_context: usize,
    pub after_context: usize,
    pub include_hidden: bool,
    pub include_ignored: bool,
    pub rg_executable: &'a Path,
}

pub(super) fn resolve_rg_executable() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(if cfg!(windows) { "rg.exe" } else { "rg" });
        if candidate.is_file() {
            return Some(candidate);
        }
        if cfg!(windows) {
            let candidate = directory.join("rg.cmd");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

pub(super) fn workspace_grep_local_rg(request: RgWorkspaceGrepRequest<'_>) -> Option<RgGrepResult> {
    let RgWorkspaceGrepRequest {
        context,
        path,
        glob_pattern,
        pattern,
        output_mode,
        file_type,
        case_insensitive,
        multiline,
        before_context,
        after_context,
        include_hidden,
        include_ignored,
        rg_executable,
    } = request;

    let base_path = context.resolve_workspace_path(path).ok()?;
    if !base_path.exists() || !base_path.is_dir() {
        return None;
    }

    let base_is_workspace_root = is_workspace_root_path(path);
    let ignored_root_names = if base_is_workspace_root && !include_ignored {
        local_ignored_root_names(&base_path)
    } else {
        Vec::new()
    };

    let mut command = Command::new(rg_executable);
    command
        .arg("--json")
        .arg("--line-number")
        .arg("--color")
        .arg("never")
        .arg("--no-messages");
    if include_hidden {
        command.arg("--hidden");
    }
    if include_ignored {
        command.arg("--no-ignore").arg("--no-ignore-vcs");
    }
    if case_insensitive {
        command.arg("-i");
    }
    if multiline {
        command.arg("--multiline").arg("--multiline-dotall");
    }
    if before_context > 0 {
        command
            .arg("--before-context")
            .arg(before_context.to_string());
    }
    if after_context > 0 {
        command
            .arg("--after-context")
            .arg(after_context.to_string());
    }
    if !glob_pattern.trim().is_empty() && glob_pattern != "**/*" {
        command.arg("--glob").arg(glob_pattern);
    }
    if base_is_workspace_root && !include_ignored {
        for root in &ignored_root_names {
            command.arg("--glob").arg(format!("!{root}/**"));
        }
    }
    if let Some(file_type) = file_type {
        for file_glob in rg_file_type_globs(file_type) {
            command.arg("--iglob").arg(file_glob);
        }
    }
    let output = command_output_with_executable_busy_retry(
        command
            .arg("--regexp")
            .arg(pattern)
            .arg(".")
            .current_dir(&base_path),
    )
    .ok()?;
    if !matches!(output.status.code(), Some(0) | Some(1) | Some(2)) {
        return None;
    }

    let mut searched_files = BTreeSet::<String>::new();
    let mut files_with_matches = BTreeSet::<String>::new();
    let mut file_counts = BTreeMap::<String, usize>::new();
    let mut line_rows = BTreeMap::<(String, u64), Value>::new();
    let mut content_rows = Vec::<Value>::new();

    let stdout = String::from_utf8_lossy(&output.stdout);
    for raw_line in stdout.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let event: Value = serde_json::from_str(line).ok()?;
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if event_type == "summary" {
            continue;
        }
        let Some(data) = event.get("data").and_then(Value::as_object) else {
            continue;
        };
        let rel_from_base = decode_rg_field(data.get("path"));
        if rel_from_base.is_empty() {
            continue;
        }
        let normalized = normalize_rg_relative_path(&rel_from_base);
        let rel_workspace =
            workspace_relative_path_or_absolute(&context.workspace, &base_path.join(normalized));
        if let Some(file_type) = file_type {
            if !matches_file_type(&rel_workspace, Some(file_type)) {
                continue;
            }
        }

        match event_type {
            "begin" => {
                searched_files.insert(rel_workspace);
            }
            "match" => {
                searched_files.insert(rel_workspace.clone());
                files_with_matches.insert(rel_workspace.clone());

                let submatches = data
                    .get("submatches")
                    .and_then(Value::as_array)
                    .filter(|items| !items.is_empty());
                let increment = submatches.map_or(1, Vec::len);
                *file_counts.entry(rel_workspace.clone()).or_insert(0) += increment;

                if output_mode != "content" {
                    continue;
                }

                let line_number = data.get("line_number").and_then(Value::as_u64).unwrap_or(1);
                let matched_lines = decode_rg_field(data.get("lines"));
                if multiline {
                    if let Some(submatches) = submatches {
                        for submatch in submatches {
                            let snippet = submatch
                                .as_object()
                                .and_then(|object| {
                                    let start = object.get("start")?.as_u64()? as usize;
                                    let end = object.get("end")?.as_u64()? as usize;
                                    substring_by_byte_range(&matched_lines, start, end)
                                })
                                .unwrap_or_else(|| {
                                    decode_rg_field(submatch.get("match")).if_empty(&matched_lines)
                                });
                            content_rows.push(json!({
                                "path": rel_workspace,
                                "line": line_number,
                                "text": snippet,
                                "is_match": true,
                            }));
                        }
                    } else {
                        content_rows.push(json!({
                            "path": rel_workspace,
                            "line": line_number,
                            "text": matched_lines,
                            "is_match": true,
                        }));
                    }
                } else {
                    let row_key = (rel_workspace.clone(), line_number);
                    let row_text = matched_lines.trim_end_matches('\n').to_string();
                    match line_rows.get_mut(&row_key) {
                        Some(existing) => {
                            existing["is_match"] = Value::Bool(true);
                            existing["text"] = Value::String(row_text);
                        }
                        None => {
                            line_rows.insert(
                                row_key,
                                json!({
                                    "path": rel_workspace,
                                    "line": line_number,
                                    "text": row_text,
                                    "is_match": true,
                                }),
                            );
                        }
                    }
                }
            }
            "context" if output_mode == "content" && !multiline => {
                searched_files.insert(rel_workspace.clone());
                let Some(line_number) = data.get("line_number").and_then(Value::as_u64) else {
                    continue;
                };
                let row_key = (rel_workspace.clone(), line_number);
                line_rows.entry(row_key).or_insert_with(|| {
                    json!({
                        "path": rel_workspace,
                        "line": line_number,
                        "text": decode_rg_field(data.get("lines")).trim_end_matches('\n'),
                        "is_match": false,
                    })
                });
            }
            _ => {}
        }
    }

    if output_mode == "content" && !multiline {
        content_rows = line_rows.into_values().collect();
    }
    if output_mode == "content" {
        content_rows.sort_by(|left, right| {
            let left_path = left["path"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let right_path = right["path"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase();
            left_path.cmp(&right_path).then_with(|| {
                left["line"]
                    .as_u64()
                    .unwrap_or_default()
                    .cmp(&right["line"].as_u64().unwrap_or_default())
            })
        });
    }

    let files_with_matches = files_with_matches.into_iter().collect::<Vec<_>>();
    let total_matches = file_counts.values().sum();
    let files_searched = if searched_files.is_empty() {
        files_with_matches.len()
    } else {
        searched_files.len()
    };

    Some(RgGrepResult {
        files_searched,
        total_matches,
        files_with_matches,
        file_counts,
        rows: content_rows,
    })
}

fn decode_rg_field(field: Option<&Value>) -> String {
    let Some(field) = field.and_then(Value::as_object) else {
        return String::new();
    };
    if let Some(text) = field.get("text").and_then(Value::as_str) {
        return text.to_string();
    }
    let Some(raw) = field.get("bytes").and_then(Value::as_str) else {
        return String::new();
    };
    base64::engine::general_purpose::STANDARD
        .decode(raw)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
        .unwrap_or_default()
}

fn normalize_rg_relative_path(path: &str) -> String {
    let mut normalized = path.replace('\\', "/");
    while normalized.starts_with("./") {
        normalized = normalized[2..].to_string();
    }
    if normalized == "." {
        String::new()
    } else {
        normalized
    }
}

trait EmptyStringFallback {
    fn if_empty(self, fallback: &str) -> String;
}

impl EmptyStringFallback for String {
    fn if_empty(self, fallback: &str) -> String {
        if self.is_empty() {
            fallback.to_string()
        } else {
            self
        }
    }
}

fn substring_by_byte_range(text: &str, start: usize, end: usize) -> Option<String> {
    if start > end
        || end > text.len()
        || !text.is_char_boundary(start)
        || !text.is_char_boundary(end)
    {
        return None;
    }
    Some(text[start..end].to_string())
}

pub(super) fn is_workspace_root_path(path: &str) -> bool {
    let normalized = path.trim();
    normalized.is_empty() || normalized.replace('\\', "/") == "."
}

pub(super) fn local_ignored_root_names(base_path: &Path) -> Vec<String> {
    let mut roots = std::fs::read_dir(base_path)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter_map(|entry| {
            let file_type = entry.file_type().ok()?;
            if !file_type.is_dir() {
                return None;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            is_ignored_root(&name.to_ascii_lowercase()).then_some(name)
        })
        .collect::<Vec<_>>();
    roots.sort();
    roots
}

fn rg_file_type_globs(file_type: &str) -> Vec<String> {
    let tokens: &[&str] = match file_type {
        "py" => &[".py", ".pyw", ".pyi"],
        "js" => &[".js", ".jsx", ".mjs"],
        "ts" => &[".ts", ".tsx"],
        "html" => &[".html", ".htm", ".xhtml"],
        "css" => &[".css", ".scss", ".sass", ".less"],
        "java" => &[".java"],
        "c" => &[".c", ".h"],
        "cpp" => &[".cpp", ".cc", ".cxx", ".c++", ".hpp", ".hh", ".hxx", ".h++"],
        "rust" => &[".rs"],
        "go" => &[".go"],
        "php" => &[".php", ".php3", ".php4", ".php5"],
        "rb" => &[".rb", ".rbx", ".rhtml", ".ruby"],
        "sh" => &[".sh", ".bash", ".zsh", ".fish"],
        "sql" => &[".sql"],
        "json" => &[".json"],
        "xml" => &[".xml", ".xsl", ".xsd"],
        "yaml" => &[".yaml", ".yml"],
        "md" => &[".md", ".markdown", ".mdown", ".mkd"],
        "txt" => &[".txt"],
        "log" => &[".log"],
        "ini" => &[".ini", ".cfg", ".conf"],
        "dockerfile" => &["dockerfile"],
        "makefile" => &["makefile", "gnumakefile"],
        _ => &[],
    };
    tokens
        .iter()
        .map(|token| {
            if token.starts_with('.') {
                format!("**/*{token}")
            } else {
                format!("**/{token}")
            }
        })
        .collect()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::tools::base::ToolContext;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;

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
    ) -> RgWorkspaceGrepRequest<'a> {
        RgWorkspaceGrepRequest {
            context,
            path: ".",
            glob_pattern: "**/*",
            pattern: "token",
            output_mode,
            file_type: Some("py"),
            case_insensitive: true,
            multiline: false,
            before_context: 0,
            after_context: 0,
            include_hidden: false,
            include_ignored: false,
            rg_executable,
        }
    }

    #[test]
    fn workspace_grep_rg_fast_path_parses_json_and_type_filter() {
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
'{"type":"summary","data":{}}'
"#,
        );

        let context = ToolContext::new(workspace.path());
        let result = workspace_grep_local_rg(rg_request(&context, &fake_rg, "files_with_matches"))
            .expect("rg result");

        assert_eq!(result.files_with_matches, vec!["a.py", "b.py"]);
        assert_eq!(result.total_matches, 2);
        assert_eq!(result.files_searched, 2);
        assert_eq!(result.file_counts["a.py"], 1);
        assert_eq!(result.file_counts["b.py"], 1);
    }

    #[test]
    fn workspace_grep_rg_fast_path_accepts_returncode_2_with_results() {
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
        let result = workspace_grep_local_rg(request).expect("rg result");

        assert_eq!(result.total_matches, 1);
        assert_eq!(result.rows[0]["path"], "a.py");
        assert_eq!(result.rows[0]["text"], "Agent from rg");
    }

    #[test]
    fn workspace_grep_rg_fast_path_returns_none_on_hard_error() {
        let workspace = tempfile::tempdir().expect("workspace");
        let fake_rg = write_fake_rg(workspace.path(), "#!/bin/sh\nexit 3\n");

        let context = ToolContext::new(workspace.path());
        let result = workspace_grep_local_rg(rg_request(&context, &fake_rg, "content"));

        assert!(result.is_none());
    }
}
