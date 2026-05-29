use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use base64::Engine as _;
use serde_json::{json, Value};

use crate::tools::base::ToolContext;
use crate::tools::common::{matches_file_type, workspace_relative_path_or_absolute};

use super::types::RgGrepResult;

pub(super) fn parse_rg_json_output(
    context: &ToolContext,
    base_path: &Path,
    output_mode: &str,
    file_type: Option<&str>,
    multiline: bool,
    stdout: &[u8],
) -> Option<RgGrepResult> {
    let mut searched_files = BTreeSet::<String>::new();
    let mut files_with_matches = BTreeSet::<String>::new();
    let mut file_counts = BTreeMap::<String, usize>::new();
    let mut line_rows = BTreeMap::<(String, u64), Value>::new();
    let mut content_rows = Vec::<Value>::new();

    let stdout = String::from_utf8_lossy(stdout);
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
