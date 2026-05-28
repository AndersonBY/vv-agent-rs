use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::path_escapes_workspace_error;
use crate::tools::common::{
    coerce_python_bool_arg, coerce_python_text_arg, collect_ignored_roots,
    command_output_with_executable_busy_retry, is_hidden_path, is_ignored_root, parse_integer_arg,
    replace_n, tool_error, workspace_relative_path_or_absolute,
};
use crate::types::{ToolArguments, ToolExecutionResult};
use crate::workspace::{glob_match, normalized_glob_pattern, LocalWorkspaceBackend};

const READ_FILE_MAX_LINES: usize = 2_000;
const READ_FILE_MAX_CHARS: usize = 50_000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct RgListFilesResult {
    files: Vec<String>,
    total_count: usize,
    truncated: bool,
    scan_limited: bool,
}

struct RgListFilesRequest<'a> {
    context: &'a ToolContext,
    base_path: &'a Path,
    base_is_workspace_root: bool,
    glob: &'a str,
    include_hidden: bool,
    include_ignored: bool,
    ignored_root_names: &'a [String],
    max_results: usize,
    scan_limit: usize,
    rg_executable: &'a Path,
}

fn resolve_rg_executable() -> Option<PathBuf> {
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

fn list_files_local_rg(request: RgListFilesRequest<'_>) -> Option<RgListFilesResult> {
    let RgListFilesRequest {
        context,
        base_path,
        base_is_workspace_root,
        glob,
        include_hidden,
        include_ignored,
        ignored_root_names,
        max_results,
        scan_limit,
        rg_executable,
    } = request;

    let mut command = Command::new(rg_executable);
    command
        .arg("--files")
        .arg("--null")
        .arg("--no-messages")
        .arg("--no-ignore")
        .arg("--no-ignore-vcs");
    if include_hidden {
        command.arg("--hidden");
    }
    if !glob.trim().is_empty() && glob != "**/*" {
        command.arg("--glob").arg(glob);
    }
    if base_is_workspace_root && !include_ignored {
        for root in ignored_root_names {
            command.arg("--glob").arg(format!("!{root}/**"));
        }
    }
    let output =
        command_output_with_executable_busy_retry(command.arg(".").current_dir(base_path)).ok()?;
    if !matches!(output.status.code(), Some(0) | Some(1)) {
        return None;
    }

    let glob_pattern = normalized_glob_pattern(glob);
    let mut files = Vec::new();
    let mut matched_count = 0usize;
    let mut scanned_count = 0usize;
    let mut scan_limited = false;

    for raw_entry in output.stdout.split(|byte| *byte == b'\0') {
        if raw_entry.is_empty() {
            continue;
        }
        scanned_count += 1;
        if scanned_count > scan_limit {
            scan_limited = true;
            break;
        }
        let rel_from_base = normalize_rg_relative_path(String::from_utf8_lossy(raw_entry));
        if rel_from_base.is_empty() || !glob_match(&rel_from_base, &glob_pattern) {
            continue;
        }
        matched_count += 1;
        if files.len() < max_results {
            let output_path = workspace_relative_path_or_absolute(
                &context.workspace,
                &base_path.join(&rel_from_base),
            );
            files.push(output_path);
        }
    }

    files.sort();
    let truncated = matched_count > files.len() || scan_limited;
    Some(RgListFilesResult {
        files,
        total_count: matched_count,
        truncated,
        scan_limited,
    })
}

fn normalize_rg_relative_path(path: std::borrow::Cow<'_, str>) -> String {
    let normalized = path.replace('\\', "/");
    normalized
        .strip_prefix("./")
        .unwrap_or(&normalized)
        .trim_start_matches('/')
        .to_string()
}

fn is_workspace_root_path(path: &str) -> bool {
    let normalized = path.trim();
    normalized.is_empty() || normalized.replace('\\', "/") == "."
}

fn local_ignored_root_names(base_path: &Path) -> Vec<String> {
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
            is_ignored_root(&name).then_some(name)
        })
        .collect::<Vec<_>>();
    roots.sort();
    roots
}

pub fn list_files(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let spec = list_files_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn list_files_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "list_files",
        "List files in the current workspace.",
        Arc::new(|context, arguments| {
            let path = coerce_python_text_arg(arguments.get("path"), ".");
            let glob = coerce_python_text_arg(arguments.get("glob"), "**/*");
            let max_results = match arguments.get("max_results") {
                Some(value) => match parse_integer_arg(value) {
                    Ok(limit) => limit.clamp(1, 5_000) as usize,
                    Err(_) => {
                        return tool_error("`max_results` and `scan_limit` must be integers");
                    }
                },
                None => 500,
            };
            let scan_limit = match arguments.get("scan_limit") {
                Some(value) => match parse_integer_arg(value) {
                    Ok(limit) => limit.max(max_results as i64) as usize,
                    Err(_) => {
                        return tool_error("`max_results` and `scan_limit` must be integers");
                    }
                },
                None => 50_000,
            };
            let include_ignored = coerce_python_bool_arg(arguments.get("include_ignored"), false);
            let include_hidden = coerce_python_bool_arg(arguments.get("include_hidden"), false);
            let resolved_path = match context.resolve_workspace_path(&path) {
                Ok(path) => path,
                Err(error) => return path_escapes_workspace_error(error),
            };
            let backend = context.effective_workspace_backend();
            let root_listing = is_workspace_root_path(&path);
            let is_local_backend = backend.as_any().is::<LocalWorkspaceBackend>();
            let rg_result = backend
                .as_any()
                .downcast_ref::<LocalWorkspaceBackend>()
                .and_then(|_| {
                    if !resolved_path.is_dir() {
                        return None;
                    }
                    let ignored_root_names = if root_listing && !include_ignored {
                        local_ignored_root_names(&resolved_path)
                    } else {
                        Vec::new()
                    };
                    let rg_executable = resolve_rg_executable()?;
                    list_files_local_rg(RgListFilesRequest {
                        context,
                        base_path: &resolved_path,
                        base_is_workspace_root: root_listing,
                        glob: &glob,
                        include_hidden,
                        include_ignored,
                        ignored_root_names: &ignored_root_names,
                        max_results,
                        scan_limit,
                        rg_executable: &rg_executable,
                    })
                    .map(|result| (result, ignored_root_names))
                });

            let list_result = if let Some((result, ignored_roots)) = rg_result {
                Ok((
                    result.files,
                    result.total_count,
                    result.truncated,
                    result.scan_limited,
                    ignored_roots,
                ))
            } else {
                match backend.list_files(&path, &glob) {
                    Ok(mut files) => {
                        let summarize_ignored_roots =
                            is_local_backend && root_listing && !include_ignored;
                        let ignored_roots = if summarize_ignored_roots {
                            collect_ignored_roots(&files)
                        } else {
                            Vec::new()
                        };
                        if summarize_ignored_roots {
                            files.retain(|path| {
                                path.split('/')
                                    .next()
                                    .is_none_or(|root| !is_ignored_root(root))
                            });
                        }
                        if !include_hidden {
                            files.retain(|path| !is_hidden_path(path));
                        }
                        let actual_count = files.len();
                        let scan_limited = actual_count > scan_limit;
                        let count = if scan_limited {
                            scan_limit
                        } else {
                            actual_count
                        };
                        let truncated = scan_limited || count > max_results;
                        let returned = files.into_iter().take(max_results).collect::<Vec<_>>();
                        Ok((returned, count, truncated, scan_limited, ignored_roots))
                    }
                    Err(error) => Err(error),
                }
            };

            match list_result {
                Ok((returned, count, truncated, scan_limited, ignored_roots)) => {
                    let returned_count = returned.len();
                    let mut payload = json!({
                        "files": returned,
                        "count": count,
                        "returned_count": returned_count,
                        "truncated": truncated,
                        "max_results": max_results,
                    });
                    if count > returned_count {
                        payload["remaining_count"] = Value::Number((count - returned_count).into());
                    }
                    if scan_limited {
                        payload["count_is_estimate"] = Value::Bool(true);
                        payload["scan_limit"] = Value::Number(scan_limit.into());
                        payload["message"] = Value::String(
                            "Listing stopped early due to scan limit. Narrow `path`/`glob` or increase `scan_limit` for more complete results."
                                .to_string(),
                        );
                    }
                    if !ignored_roots.is_empty() {
                        payload["ignored_roots"] = Value::Array(
                            ignored_roots
                                .into_iter()
                                .map(|path| json!({"path": path}))
                                .collect(),
                        );
                        let ignored_message =
                            "Common dependency/cache directories are summarized by default. List those directories explicitly when needed.";
                        payload["message"] = Value::String(
                            payload
                                .get("message")
                                .and_then(Value::as_str)
                                .map(|message| format!("{message} {ignored_message}"))
                                .unwrap_or_else(|| ignored_message.to_string()),
                        );
                    }
                    crate::types::ToolExecutionResult::success("", payload.to_string())
                }
                Err(error) => workspace_backend_error(error),
            }
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("list_files") {
        spec.schema = schema;
    }
    spec
}

pub fn file_info(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let spec = file_info_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn file_info_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "file_info",
        "Return metadata for a workspace path.",
        Arc::new(|context, arguments| {
            if !arguments.contains_key("path") {
                return tool_error("missing required argument: path");
            }
            let path = coerce_python_text_arg(arguments.get("path"), "");
            if let Err(error) = context.resolve_workspace_path(&path) {
                return path_escapes_workspace_error(error);
            }
            let backend = context.effective_workspace_backend();
            match backend.file_info(&path) {
                Ok(Some(info)) => {
                    let mut payload = json!({
                        "path": info.path,
                        "exists": true,
                        "is_file": info.is_file,
                        "is_dir": info.is_dir,
                        "size": info.size,
                        "modified_at": info.modified_at,
                    });
                    if info.is_file {
                        payload["suffix"] = Value::String(info.suffix);
                    }
                    crate::types::ToolExecutionResult::success("", payload.to_string())
                }
                Ok(None) => tool_error(format!("path not found: {path}")),
                Err(error) => workspace_backend_error(error),
            }
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("file_info") {
        spec.schema = schema;
    }
    spec
}

pub fn read_file(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let spec = read_file_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn read_file_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "read_file",
        "Read a text file from the current workspace.",
        Arc::new(|context, arguments| {
            if !arguments.contains_key("path") {
                return tool_error("missing required argument: path");
            }
            let path = coerce_python_text_arg(arguments.get("path"), "");
            if let Err(error) = context.resolve_workspace_path(&path) {
                return path_escapes_workspace_error(error);
            }
            let backend = context.effective_workspace_backend();
            match backend.file_info(&path) {
                Ok(Some(info)) if info.is_file => {}
                Ok(_) => return tool_error(format!("file not found: {path}")),
                Err(error) => return workspace_backend_error(error),
            }
            let start_line = match arguments.get("start_line") {
                Some(value) => match parse_integer_arg(value) {
                    Ok(line) => line.max(1) as usize,
                    Err(_) => return tool_error("`start_line`/`end_line` must be integers"),
                },
                None => 1,
            };
            let end_line = match arguments.get("end_line") {
                Some(value) => match parse_integer_arg(value) {
                    Ok(line) => Some(line.max(start_line as i64) as usize),
                    Err(_) => return tool_error("`start_line`/`end_line` must be integers"),
                },
                None => None,
            };
            let show_line_numbers =
                coerce_python_bool_arg(arguments.get("show_line_numbers"), false);
            match backend.read_text(&path) {
                Ok(text) => {
                    let lines = text.lines().collect::<Vec<_>>();
                    let requested_start_index = start_line.saturating_sub(1);
                    let slice_start_index = requested_start_index.min(lines.len());
                    let slice_end_index = end_line
                        .unwrap_or(lines.len())
                        .min(lines.len())
                        .max(slice_start_index);
                    let selected = &lines[slice_start_index..slice_end_index];
                    let selected_line_count = selected.len();
                    let actual_start_line = requested_start_index + 1;
                    let actual_end_line = requested_start_index + selected_line_count;
                    let content = selected
                        .iter()
                        .enumerate()
                        .map(|(offset, line)| {
                            if show_line_numbers {
                                format!("{}: {line}", actual_start_line + offset)
                            } else {
                                (*line).to_string()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if selected_line_count > READ_FILE_MAX_LINES
                        || content.len() > READ_FILE_MAX_CHARS
                    {
                        let total_lines = lines.len();
                        let total_chars = text.len();
                        let suggested_start = start_line.min(total_lines.max(1));
                        let suggested_end =
                            (suggested_start + READ_FILE_MAX_LINES - 1).min(total_lines);
                        return crate::types::ToolExecutionResult::success(
                            "",
                            json!({
                                "path": path,
                                "start_line": actual_start_line,
                                "end_line": actual_end_line,
                                "show_line_numbers": show_line_numbers,
                                "content": Value::Null,
                                "file_info": {
                                    "total_lines": total_lines,
                                    "total_chars": total_chars,
                                },
                                "requested": {
                                    "line_count": selected_line_count,
                                    "char_count": content.len(),
                                },
                                "limits": {
                                    "max_lines": READ_FILE_MAX_LINES,
                                    "max_chars": READ_FILE_MAX_CHARS,
                                },
                                "suggested_range": {
                                    "start_line": suggested_start,
                                    "end_line": suggested_end,
                                },
                                "message": "Requested read exceeds limits. Use start_line/end_line for a smaller range.",
                            })
                            .to_string(),
                        );
                    }
                    crate::types::ToolExecutionResult::success(
                        "",
                        json!({
                            "path": path,
                            "start_line": actual_start_line,
                            "end_line": actual_end_line,
                            "show_line_numbers": show_line_numbers,
                            "content": content,
                        })
                        .to_string(),
                    )
                }
                Err(error) => workspace_backend_error(error),
            }
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("read_file") {
        spec.schema = schema;
    }
    spec
}

pub fn write_file(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let spec = write_file_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn write_file_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "write_file",
        "Write a text file in the current workspace.",
        Arc::new(|context, arguments| {
            if !arguments.contains_key("path") {
                return tool_error("missing required argument: path");
            }
            let path = coerce_python_text_arg(arguments.get("path"), "");
            if let Err(error) = context.resolve_workspace_path(&path) {
                return path_escapes_workspace_error(error);
            }
            let backend = context.effective_workspace_backend();
            let content = coerce_python_text_arg(arguments.get("content"), "");
            let append = coerce_python_bool_arg(arguments.get("append"), false);
            let leading_newline =
                append && coerce_python_bool_arg(arguments.get("leading_newline"), false);
            let trailing_newline =
                append && coerce_python_bool_arg(arguments.get("trailing_newline"), false);
            let write_content = format!(
                "{}{}{}",
                if leading_newline { "\n" } else { "" },
                content.as_str(),
                if trailing_newline { "\n" } else { "" }
            );
            match backend.write_text(&path, &write_content, append) {
                Ok(written) => crate::types::ToolExecutionResult::success(
                    "",
                    json!({
                        "ok": true,
                        "path": path,
                        "append": append,
                        "leading_newline": leading_newline,
                        "trailing_newline": trailing_newline,
                        "written_chars": written,
                    })
                    .to_string(),
                ),
                Err(error) => workspace_backend_error(error),
            }
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("write_file") {
        spec.schema = schema;
    }
    spec
}

pub fn file_str_replace(
    context: &mut ToolContext,
    arguments: &ToolArguments,
) -> ToolExecutionResult {
    let spec = file_str_replace_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn file_str_replace_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "file_str_replace",
        "Replace text in a workspace file.",
        Arc::new(|context, arguments| {
            if !arguments.contains_key("path") {
                return tool_error("missing required argument: path");
            }
            let path = coerce_python_text_arg(arguments.get("path"), "");
            if let Err(error) = context.resolve_workspace_path(&path) {
                return path_escapes_workspace_error(error);
            }
            let backend = context.effective_workspace_backend();
            match backend.file_info(&path) {
                Ok(Some(info)) if info.is_file => {}
                Ok(_) => return tool_error(format!("file not found: {path}")),
                Err(error) => return workspace_backend_error(error),
            }
            let old_str = coerce_python_text_arg(arguments.get("old_str"), "");
            if old_str.is_empty() {
                return tool_error("`old_str` cannot be empty");
            }
            let new_str = coerce_python_text_arg(arguments.get("new_str"), "");
            let replace_all = coerce_python_bool_arg(arguments.get("replace_all"), false);
            let max_replacements = match arguments.get("max_replacements") {
                Some(value) => match parse_integer_arg(value) {
                    Ok(limit) => limit.max(1) as usize,
                    Err(_) => return tool_error("`max_replacements` must be an integer"),
                },
                None => 1,
            };
            match backend.read_text(&path) {
                Ok(text) => {
                    let occurrence_count = text.matches(&old_str).count();
                    if occurrence_count == 0 {
                        return tool_error("`old_str` not found in file");
                    }
                    let replaced_count = if replace_all {
                        occurrence_count
                    } else {
                        occurrence_count.min(max_replacements)
                    };
                    let replaced_text = if replace_all {
                        text.replace(&old_str, &new_str)
                    } else {
                        replace_n(&text, &old_str, &new_str, max_replacements)
                    };
                    match backend.write_text(&path, &replaced_text, false) {
                        Ok(_) => crate::types::ToolExecutionResult::success(
                            "",
                            json!({
                                "ok": true,
                                "path": path,
                                "replaced_count": replaced_count,
                            })
                            .to_string(),
                        ),
                        Err(error) => workspace_backend_error(error),
                    }
                }
                Err(error) => workspace_backend_error(error),
            }
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("file_str_replace") {
        spec.schema = schema;
    }
    spec
}

fn workspace_backend_error(error: std::io::Error) -> ToolExecutionResult {
    if error.kind() == ErrorKind::PermissionDenied
        && error.to_string().contains("Path escapes workspace")
    {
        return path_escapes_workspace_error(error.to_string());
    }
    tool_error(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::base::ToolContext;

    #[cfg(unix)]
    fn write_fake_rg(workspace: &Path, script: &str) -> PathBuf {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;

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

    #[cfg(unix)]
    #[test]
    fn list_files_rg_fast_path_normalizes_dot_slash_glob_matches() {
        let workspace = tempfile::tempdir().expect("workspace");
        let fake_rg = write_fake_rg(
            workspace.path(),
            "#!/bin/sh\nprintf './doc.md\\0./nested/inner.md\\0note.txt\\0'\n",
        );

        let context = ToolContext::new(workspace.path());
        let result = list_files_local_rg(RgListFilesRequest {
            context: &context,
            base_path: workspace.path(),
            base_is_workspace_root: true,
            glob: "*.md",
            include_hidden: false,
            include_ignored: false,
            ignored_root_names: &[],
            max_results: 10,
            scan_limit: 100,
            rg_executable: &fake_rg,
        })
        .expect("rg result");

        assert_eq!(result.files, vec!["doc.md"]);
        assert_eq!(result.total_count, 1);
        assert!(!result.truncated);
        assert!(!result.scan_limited);
    }

    #[cfg(unix)]
    #[test]
    fn list_files_rg_scan_limited_count_reports_matched_items_like_python() {
        let workspace = tempfile::tempdir().expect("workspace");
        let fake_rg = write_fake_rg(
            workspace.path(),
            "#!/bin/sh\nprintf 'a.txt\\0b.txt\\0doc.md\\0late.md\\0'\n",
        );

        let context = ToolContext::new(workspace.path());
        let result = list_files_local_rg(RgListFilesRequest {
            context: &context,
            base_path: workspace.path(),
            base_is_workspace_root: true,
            glob: "*.md",
            include_hidden: false,
            include_ignored: false,
            ignored_root_names: &[],
            max_results: 10,
            scan_limit: 3,
            rg_executable: &fake_rg,
        })
        .expect("rg result");

        assert_eq!(result.files, vec!["doc.md"]);
        assert_eq!(result.total_count, 1);
        assert!(result.truncated);
        assert!(result.scan_limited);
    }
}
