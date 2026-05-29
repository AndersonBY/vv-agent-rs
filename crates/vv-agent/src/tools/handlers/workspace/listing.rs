use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::path_escapes_workspace_error;
use crate::tools::common::{
    coerce_truthy_arg, collect_ignored_roots, command_output_with_executable_busy_retry,
    is_hidden_path, is_ignored_root, parse_integer_arg, stringify_tool_arg, tool_error,
    workspace_relative_path_or_absolute,
};
use crate::types::{ToolArguments, ToolExecutionResult};
use crate::workspace::{glob_match, normalized_glob_pattern, LocalWorkspaceBackend};

use super::workspace_backend_error;

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
            let path = stringify_tool_arg(arguments.get("path"), ".");
            let glob = stringify_tool_arg(arguments.get("glob"), "**/*");
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
            let include_ignored = coerce_truthy_arg(arguments.get("include_ignored"), false);
            let include_hidden = coerce_truthy_arg(arguments.get("include_hidden"), false);
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
    if let Some(schema) = crate::tools::schemas::schema_for("list_files") {
        spec.schema = schema;
    }
    spec
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
    fn list_files_rg_scan_limited_count_reports_matched_items() {
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
