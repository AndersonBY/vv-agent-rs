use std::path::Path;

use serde_json::Value;

use crate::tools::base::ToolContext;
use crate::tools::common::{matches_file_type, workspace_relative_path_or_absolute};

use super::decode::decode_rg_field;
use super::paths::normalize_rg_relative_path;

pub(super) enum RgJsonEvent {
    Summary {
        searches: Option<usize>,
    },
    Begin {
        path: String,
    },
    Match {
        path: String,
        line_number: u64,
        matched_lines: String,
        submatches: Vec<RgSubmatch>,
    },
    Context {
        path: String,
        line_number: u64,
        lines: String,
    },
}

pub(super) struct RgSubmatch {
    pub start: Option<usize>,
    pub end: Option<usize>,
    pub matched_text: String,
}

impl RgJsonEvent {
    pub fn from_value(
        context: &ToolContext,
        base_path: &Path,
        file_type: Option<&str>,
        event: &Value,
    ) -> Option<Self> {
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let data = event.get("data").and_then(Value::as_object)?;
        if event_type == "summary" {
            return Some(Self::Summary {
                searches: data
                    .get("stats")
                    .and_then(Value::as_object)
                    .and_then(|stats| stats.get("searches"))
                    .and_then(Value::as_u64)
                    .map(|value| value as usize),
            });
        }
        let path = event_workspace_path(context, base_path, file_type, data.get("path"))?;
        match event_type {
            "begin" => Some(Self::Begin { path }),
            "match" => Some(Self::Match {
                path,
                line_number: data.get("line_number").and_then(Value::as_u64).unwrap_or(1),
                matched_lines: decode_rg_field(data.get("lines")),
                submatches: parse_submatches(data.get("submatches")),
            }),
            "context" => {
                let line_number = data.get("line_number").and_then(Value::as_u64)?;
                Some(Self::Context {
                    path,
                    line_number,
                    lines: decode_rg_field(data.get("lines")),
                })
            }
            _ => None,
        }
    }
}

fn event_workspace_path(
    context: &ToolContext,
    base_path: &Path,
    file_type: Option<&str>,
    path_field: Option<&Value>,
) -> Option<String> {
    let rel_from_base = decode_rg_field(path_field);
    if rel_from_base.is_empty() {
        return None;
    }
    let normalized = normalize_rg_relative_path(&rel_from_base);
    let rel_workspace =
        workspace_relative_path_or_absolute(&context.workspace, &base_path.join(normalized));
    if let Some(file_type) = file_type {
        if !matches_file_type(&rel_workspace, Some(file_type)) {
            return None;
        }
    }
    Some(rel_workspace)
}

fn parse_submatches(value: Option<&Value>) -> Vec<RgSubmatch> {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(parse_submatch).collect())
        .unwrap_or_default()
}

fn parse_submatch(value: &Value) -> Option<RgSubmatch> {
    let object = value.as_object()?;
    Some(RgSubmatch {
        start: object
            .get("start")
            .and_then(Value::as_u64)
            .map(|value| value as usize),
        end: object
            .get("end")
            .and_then(Value::as_u64)
            .map(|value| value as usize),
        matched_text: decode_rg_field(object.get("match")),
    })
}
