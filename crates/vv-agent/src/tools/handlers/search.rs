use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use base64::Engine as _;
use regex::RegexBuilder;
use serde_json::{json, Value};

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{
    coerce_python_text_arg, command_output_with_executable_busy_retry, grep_text, is_hidden_path,
    is_ignored_root, is_supported_file_type, matches_file_type, parse_integer_arg,
    path_escapes_workspace_error, supported_file_types_message,
    workspace_relative_path_or_absolute, GrepTextOptions,
};
use crate::types::{ToolArguments, ToolDirective, ToolExecutionResult, ToolResultStatus};
use crate::workspace::{normalized_glob_pattern, LocalWorkspaceBackend};

const MAX_STRUCTURED_ITEMS: usize = 200;
const MAX_STRUCTURED_CHARS: usize = 20_000;
const MAX_RESULT_LINES: usize = 500;
const MAX_RESULT_CHARS: usize = 30_000;

#[derive(Debug, Clone, PartialEq, Eq)]
struct RgGrepResult {
    files_searched: usize,
    total_matches: usize,
    files_with_matches: Vec<String>,
    file_counts: BTreeMap<String, usize>,
    rows: Vec<Value>,
}

struct RgWorkspaceGrepRequest<'a> {
    context: &'a ToolContext,
    path: &'a str,
    glob_pattern: &'a str,
    pattern: &'a str,
    output_mode: &'a str,
    file_type: Option<&'a str>,
    case_insensitive: bool,
    multiline: bool,
    before_context: usize,
    after_context: usize,
    include_hidden: bool,
    include_ignored: bool,
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

fn workspace_grep_local_rg(request: RgWorkspaceGrepRequest<'_>) -> Option<RgGrepResult> {
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

pub fn workspace_grep(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let spec = workspace_grep_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn workspace_grep_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "workspace_grep",
        "Search workspace files with grep-style semantics.",
        Arc::new(|context, arguments| {
            let pattern = coerce_python_text_arg(arguments.get("pattern"), "")
                .trim()
                .to_string();
            if pattern.is_empty() {
                return grep_error("Search pattern is required");
            }
            let output_mode = coerce_python_text_arg(arguments.get("output_mode"), "content");
            if !matches!(
                output_mode.as_str(),
                "content" | "files_with_matches" | "count"
            ) {
                return grep_error(format!(
                    "Invalid `output_mode`: {output_mode}. Supported: content, count, files_with_matches"
                ));
            }
            let file_type = arguments
                .get("type")
                .map(|value| coerce_python_text_arg(Some(value), ""))
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty());
            if let Some(file_type) = &file_type {
                if !is_supported_file_type(file_type) {
                    return grep_error(format!(
                        "Unsupported file type: {file_type}. Supported types: {}",
                        supported_file_types_message()
                    ));
                }
            }
            let path = coerce_python_text_arg(arguments.get("path"), ".");
            let glob = coerce_python_text_arg(arguments.get("glob"), "**/*");
            let glob_pattern = normalized_glob_pattern(&glob);
            let include_hidden = arguments
                .get("include_hidden")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let include_ignored = arguments
                .get("include_ignored")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let multiline = arguments
                .get("multiline")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let show_line_numbers = arguments.get("n").and_then(Value::as_bool).unwrap_or(true);
            let parse_optional_usize =
                |name: &str, min_value: i64| -> Result<Option<usize>, String> {
                    match arguments.get(name) {
                        Some(value) => parse_integer_arg(value)
                            .map(|parsed| Some(parsed.max(min_value) as usize))
                            .map_err(|_| format!("`{name}` must be an integer")),
                        None => Ok(None),
                    }
                };
            let context_lines = match parse_optional_usize("c", 0) {
                Ok(value) => value,
                Err(error) => return grep_error(error),
            };
            let before_context = match context_lines {
                Some(value) => value,
                None => match parse_optional_usize("b", 0) {
                    Ok(value) => value.unwrap_or(0),
                    Err(error) => return grep_error(error),
                },
            };
            let after_context = match context_lines {
                Some(value) => value,
                None => match parse_optional_usize("a", 0) {
                    Ok(value) => value.unwrap_or(0),
                    Err(error) => return grep_error(error),
                },
            };
            let head_limit_raw = arguments
                .get("head_limit")
                .or_else(|| arguments.get("max_results"));
            let head_limit = match head_limit_raw {
                Some(value) => match parse_integer_arg(value) {
                    Ok(parsed) => Some(parsed.max(1) as usize),
                    Err(_) => return grep_error("`head_limit` must be an integer"),
                },
                None => None,
            };
            let case_insensitive = if let Some(case_sensitive) =
                arguments.get("case_sensitive").and_then(Value::as_bool)
            {
                !case_sensitive
            } else if let Some(force_insensitive) = arguments.get("i").and_then(Value::as_bool) {
                force_insensitive
            } else {
                !pattern.chars().any(char::is_uppercase)
            };
            let regex = match RegexBuilder::new(&pattern)
                .case_insensitive(case_insensitive)
                .multi_line(multiline)
                .dot_matches_new_line(multiline)
                .build()
            {
                Ok(regex) => regex,
                Err(error) => {
                    return grep_error(format!("Invalid regular expression: {error}"));
                }
            };

            if let Err(error) = context.resolve_workspace_path(&path) {
                return path_escapes_workspace_error(error);
            }
            let backend = context.effective_workspace_backend();
            let explicit_file_target = backend.is_file(&path);
            let mut searched_files = 0usize;
            let mut total_matches = 0usize;
            let mut files_with_matches = Vec::<String>::new();
            let mut file_counts = BTreeMap::<String, usize>::new();
            let mut rows = Vec::<Value>::new();

            let rg_result = if explicit_file_target {
                None
            } else {
                backend
                    .as_any()
                    .downcast_ref::<LocalWorkspaceBackend>()
                    .and_then(|_| {
                        let rg_executable = resolve_rg_executable()?;
                        workspace_grep_local_rg(RgWorkspaceGrepRequest {
                            context,
                            path: &path,
                            glob_pattern: &glob_pattern,
                            pattern: &pattern,
                            output_mode: &output_mode,
                            file_type: file_type.as_deref(),
                            case_insensitive,
                            multiline,
                            before_context,
                            after_context,
                            include_hidden,
                            include_ignored,
                            rg_executable: &rg_executable,
                        })
                    })
            };

            if let Some(result) = rg_result {
                searched_files = result.files_searched;
                total_matches = result.total_matches;
                files_with_matches = result.files_with_matches;
                file_counts = result.file_counts;
                rows = result.rows;
            } else {
                let candidate_files = if explicit_file_target {
                    let display_path = backend
                        .file_info(&path)
                        .ok()
                        .flatten()
                        .map(|info| info.path)
                        .unwrap_or_else(|| path.replace('\\', "/"));
                    vec![(path.clone(), display_path)]
                } else {
                    match backend.list_files(&path, &glob_pattern) {
                        Ok(files) => files
                            .into_iter()
                            .map(|file_path| (file_path.clone(), file_path))
                            .collect(),
                        Err(error) => return grep_error(error.to_string()),
                    }
                };

                for (read_path, relative_path) in candidate_files {
                    if !explicit_file_target && !include_hidden && is_hidden_path(&relative_path) {
                        continue;
                    }
                    if !explicit_file_target
                        && !include_ignored
                        && is_workspace_root_path(&path)
                        && relative_path.split('/').next().is_some_and(is_ignored_root)
                    {
                        continue;
                    }
                    if !matches_file_type(&relative_path, file_type.as_deref()) {
                        continue;
                    }
                    let Ok(text) = backend.read_text(&read_path) else {
                        continue;
                    };
                    searched_files += 1;
                    let grep_options = GrepTextOptions {
                        multiline,
                        before_context,
                        after_context,
                        show_line_numbers,
                    };
                    let grep_result = grep_text(&relative_path, &text, &regex, grep_options);
                    let match_count = grep_result.match_count;
                    if match_count == 0 {
                        continue;
                    }
                    total_matches += match_count;
                    files_with_matches.push(relative_path.clone());
                    file_counts.insert(relative_path, match_count);
                    rows.extend(grep_result.rows);
                }
            }

            files_with_matches.sort();
            let files_with_match_count = files_with_matches.len();
            let total_result_items = match output_mode.as_str() {
                "files_with_matches" => files_with_matches.len(),
                "count" => file_counts.len(),
                _ => rows.len(),
            };
            let mut head_limited = false;
            let structured_capped;
            if let Some(limit) = head_limit {
                match output_mode.as_str() {
                    "files_with_matches" => {
                        head_limited = files_with_matches.len() > limit;
                        files_with_matches.truncate(limit);
                    }
                    "count" => {
                        head_limited = file_counts.len() > limit;
                        if head_limited {
                            file_counts = file_counts.into_iter().take(limit).collect();
                        }
                    }
                    _ => {
                        head_limited = rows.len() > limit;
                        rows.truncate(limit);
                    }
                }
            }
            match output_mode.as_str() {
                "files_with_matches" => {
                    let (capped_files, capped) = cap_structured_items(files_with_matches, |path| {
                        estimate_file_path_size(path)
                    });
                    files_with_matches = capped_files;
                    structured_capped = capped;
                }
                "count" => {
                    let count_items = file_counts.into_iter().collect::<Vec<_>>();
                    let (capped_items, capped) =
                        cap_structured_items(count_items, estimate_file_count_size);
                    file_counts = capped_items.into_iter().collect();
                    structured_capped = capped;
                }
                _ => {
                    let (capped_rows, capped) = cap_structured_items(rows, estimate_match_row_size);
                    rows = capped_rows;
                    structured_capped = capped;
                }
            }
            let structured_truncated = head_limited || structured_capped;

            let summary = json!({
                "files_searched": searched_files,
                "files_with_matches": files_with_match_count,
                "total_matches": total_matches,
            });
            let mut payload = json!({
                "summary": summary,
                "pattern": pattern,
                "output_mode": output_mode.clone(),
                "head_limit": head_limit,
                "head_limited": head_limited,
                "total_result_items": total_result_items,
                "returned_count": match output_mode.as_str() {
                    "files_with_matches" => files_with_matches.len(),
                    "count" => file_counts.len(),
                    _ => rows.len(),
                },
                "content_truncated": false,
                "structured_truncated": structured_truncated,
                "truncated": structured_truncated,
            });
            if structured_capped {
                payload["structured_item_limit"] = json!(MAX_STRUCTURED_ITEMS);
                payload["structured_char_limit"] = json!(MAX_STRUCTURED_CHARS);
            }
            match output_mode.as_str() {
                "files_with_matches" => payload["files"] = json!(files_with_matches),
                "count" => payload["file_counts"] = json!(file_counts),
                _ => payload["matches"] = Value::Array(rows),
            }
            let content = render_grep_content(
                &output_mode,
                &pattern,
                &payload,
                show_line_numbers,
                structured_truncated,
            );
            let (content, content_truncated) =
                truncate_result_text(content, total_matches, files_with_match_count);
            payload["content_truncated"] = json!(content_truncated);
            payload["truncated"] = json!(content_truncated || structured_truncated);
            let metadata = payload
                .as_object()
                .map(|object| {
                    object
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect()
                })
                .unwrap_or_default();
            ToolExecutionResult {
                tool_call_id: String::new(),
                content,
                status: ToolResultStatus::Success,
                directive: ToolDirective::Continue,
                error_code: None,
                metadata,
                image_url: None,
                image_path: None,
            }
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("workspace_grep") {
        spec.schema = schema;
    }
    spec
}

fn render_grep_content(
    output_mode: &str,
    pattern: &str,
    payload: &Value,
    show_line_numbers: bool,
    head_limited: bool,
) -> String {
    let summary = &payload["summary"];
    let total_matches = summary["total_matches"].as_u64().unwrap_or_default();
    let files_with_matches = summary["files_with_matches"].as_u64().unwrap_or_default();
    match output_mode {
        "files_with_matches" => {
            let files = payload["files"].as_array().cloned().unwrap_or_default();
            let mut lines = vec![format!(
                "Found {files_with_matches} files matching pattern {pattern:?}"
            )];
            if files.is_empty() {
                lines.push("No matches found.".to_string());
            } else {
                if head_limited {
                    lines.push(format!("Showing first {} files.", files.len()));
                }
                lines.extend(
                    files
                        .into_iter()
                        .filter_map(|file| file.as_str().map(str::to_string)),
                );
            }
            lines.join("\n")
        }
        "count" => {
            let mut lines = vec![format!("Match counts for pattern {pattern:?}")];
            if head_limited {
                lines.push(format!(
                    "Showing first {} files.",
                    payload["file_counts"]
                        .as_object()
                        .map_or(0, |items| items.len())
                ));
            }
            if let Some(counts) = payload["file_counts"].as_object() {
                for (file, count) in counts {
                    lines.push(format!("{}: {}", file, count.as_u64().unwrap_or_default()));
                }
            }
            lines.push(format!(
                "Total: {total_matches} matches in {files_with_matches} files"
            ));
            lines.join("\n")
        }
        _ => {
            let mut lines = vec![format!(
                "Found {total_matches} matches in {files_with_matches} files for pattern {pattern:?}"
            )];
            let rows = payload["matches"].as_array().cloned().unwrap_or_default();
            if rows.is_empty() {
                lines.push("No matches found.".to_string());
                return lines.join("\n");
            }
            if head_limited {
                lines.push(format!("Showing first {} rows.", rows.len()));
            }
            let mut current_file = String::new();
            for row in rows {
                let row_path = row["path"].as_str().unwrap_or_default();
                if current_file != row_path {
                    lines.push(format!("File: {row_path}"));
                    current_file = row_path.to_string();
                }
                let marker = if row["is_match"].as_bool().unwrap_or(false) {
                    ""
                } else {
                    "-"
                };
                let text = row["text"].as_str().unwrap_or_default();
                if show_line_numbers {
                    let line = row["line"].as_u64().unwrap_or_default();
                    lines.push(format!("  {marker}{line}: {text}"));
                } else {
                    lines.push(format!("  {marker}{text}"));
                }
            }
            lines.join("\n")
        }
    }
}

fn grep_error(message: impl Into<String>) -> ToolExecutionResult {
    let message = message.into();
    ToolExecutionResult {
        tool_call_id: String::new(),
        content: message.clone(),
        status: ToolResultStatus::Error,
        directive: ToolDirective::Continue,
        error_code: None,
        metadata: BTreeMap::from([("error".to_string(), Value::String(message))]),
        image_url: None,
        image_path: None,
    }
}

fn truncate_result_text(
    result_text: String,
    total_matches: usize,
    files_with_matches: usize,
) -> (String, bool) {
    let line_count = result_text.lines().count();
    if line_count <= MAX_RESULT_LINES && result_text.len() <= MAX_RESULT_CHARS {
        return (result_text, false);
    }

    let truncated = if result_text.len() > MAX_RESULT_CHARS {
        let mut end = MAX_RESULT_CHARS.min(result_text.len());
        while !result_text.is_char_boundary(end) {
            end -= 1;
        }
        let candidate = &result_text[..end];
        match candidate.rfind('\n') {
            Some(last_newline) if last_newline > MAX_RESULT_CHARS * 4 / 5 => {
                candidate[..last_newline].to_string()
            }
            _ => candidate.to_string(),
        }
    } else {
        result_text
            .lines()
            .take(MAX_RESULT_LINES)
            .collect::<Vec<_>>()
            .join("\n")
    };

    let shown_lines = truncated.lines().count();
    let truncated_info = format!(
        "\n\n--- TRUNCATED ---\n\
         Shown: {shown_lines} lines, {} characters\n\
         Total matches: {total_matches} in {files_with_matches} files\n\
         Use a narrower pattern/path/glob/type/head_limit for more focused output.",
        truncated.len()
    );
    (format!("{truncated}{truncated_info}"), true)
}

fn estimate_match_row_size(row: &Value) -> usize {
    row["path"].as_str().map_or(0, str::len)
        + row["line"]
            .as_u64()
            .map_or(0, |line| line.to_string().len())
        + row["text"].as_str().map_or(0, str::len)
        + 32
}

fn estimate_file_path_size(path: &str) -> usize {
    path.len() + 4
}

fn estimate_file_count_size((path, count): &(String, usize)) -> usize {
    path.len() + count.to_string().len() + 8
}

fn cap_structured_items<T>(items: Vec<T>, estimator: impl Fn(&T) -> usize) -> (Vec<T>, bool) {
    let mut capped = Vec::new();
    let mut used_chars = 0usize;

    for item in items {
        let item_size = estimator(&item).max(1);
        if !capped.is_empty()
            && (capped.len() >= MAX_STRUCTURED_ITEMS
                || used_chars.saturating_add(item_size) > MAX_STRUCTURED_CHARS)
        {
            return (capped, true);
        }
        capped.push(item);
        used_chars = used_chars.saturating_add(item_size);
    }

    (capped, false)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::tools::base::ToolContext;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn write_fake_rg(workspace: &Path, script: &str) -> std::path::PathBuf {
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
