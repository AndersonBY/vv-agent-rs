use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use super::decode::{if_empty, substring_by_byte_range};
use super::events::{RgJsonEvent, RgSubmatch};
use crate::tools::handlers::search::local_rg::types::RgGrepResult;

pub(super) struct RgJsonState {
    output_mode: String,
    multiline: bool,
    searched_files: BTreeSet<String>,
    files_with_matches: BTreeSet<String>,
    file_counts: BTreeMap<String, usize>,
    line_rows: BTreeMap<(String, u64), Value>,
    content_rows: Vec<Value>,
}

impl RgJsonState {
    pub fn new(output_mode: &str, multiline: bool) -> Self {
        Self {
            output_mode: output_mode.to_string(),
            multiline,
            searched_files: BTreeSet::new(),
            files_with_matches: BTreeSet::new(),
            file_counts: BTreeMap::new(),
            line_rows: BTreeMap::new(),
            content_rows: Vec::new(),
        }
    }

    pub fn record(&mut self, event: RgJsonEvent) {
        match event {
            RgJsonEvent::Begin { path } => {
                self.searched_files.insert(path);
            }
            RgJsonEvent::Match {
                path,
                line_number,
                matched_lines,
                submatches,
            } => self.record_match(path, line_number, matched_lines, submatches),
            RgJsonEvent::Context {
                path,
                line_number,
                lines,
            } => self.record_context(path, line_number, lines),
        }
    }

    pub fn finish(mut self) -> RgGrepResult {
        if self.output_mode == "content" && !self.multiline {
            self.content_rows = self.line_rows.into_values().collect();
        }
        if self.output_mode == "content" {
            self.content_rows.sort_by(compare_rows);
        }

        let files_with_matches = self.files_with_matches.into_iter().collect::<Vec<_>>();
        let total_matches = self.file_counts.values().sum();
        let files_searched = if self.searched_files.is_empty() {
            files_with_matches.len()
        } else {
            self.searched_files.len()
        };

        RgGrepResult {
            files_searched,
            total_matches,
            files_with_matches,
            file_counts: self.file_counts,
            rows: self.content_rows,
        }
    }

    fn record_match(
        &mut self,
        path: String,
        line_number: u64,
        matched_lines: String,
        submatches: Vec<RgSubmatch>,
    ) {
        self.searched_files.insert(path.clone());
        self.files_with_matches.insert(path.clone());
        let increment = if submatches.is_empty() {
            1
        } else {
            submatches.len()
        };
        *self.file_counts.entry(path.clone()).or_insert(0) += increment;

        if self.output_mode != "content" {
            return;
        }
        if self.multiline {
            self.record_multiline_match(path, line_number, matched_lines, submatches);
        } else {
            self.record_line_match(path, line_number, matched_lines);
        }
    }

    fn record_multiline_match(
        &mut self,
        path: String,
        line_number: u64,
        matched_lines: String,
        submatches: Vec<RgSubmatch>,
    ) {
        if submatches.is_empty() {
            self.content_rows
                .push(match_row(path, line_number, matched_lines));
            return;
        }
        for submatch in submatches {
            let snippet = submatch
                .start
                .zip(submatch.end)
                .and_then(|(start, end)| substring_by_byte_range(&matched_lines, start, end))
                .unwrap_or_else(|| if_empty(submatch.matched_text, &matched_lines));
            self.content_rows
                .push(match_row(path.clone(), line_number, snippet));
        }
    }

    fn record_line_match(&mut self, path: String, line_number: u64, matched_lines: String) {
        let row_key = (path.clone(), line_number);
        let row_text = matched_lines.trim_end_matches('\n').to_string();
        match self.line_rows.get_mut(&row_key) {
            Some(existing) => {
                existing["is_match"] = Value::Bool(true);
                existing["text"] = Value::String(row_text);
            }
            None => {
                self.line_rows
                    .insert(row_key, match_row(path, line_number, row_text));
            }
        }
    }

    fn record_context(&mut self, path: String, line_number: u64, lines: String) {
        if self.output_mode != "content" || self.multiline {
            return;
        }
        self.searched_files.insert(path.clone());
        let row_key = (path.clone(), line_number);
        self.line_rows.entry(row_key).or_insert_with(|| {
            json!({
                "path": path,
                "line": line_number,
                "text": lines.trim_end_matches('\n'),
                "is_match": false,
            })
        });
    }
}

fn match_row(path: String, line_number: u64, text: String) -> Value {
    json!({
        "path": path,
        "line": line_number,
        "text": text,
        "is_match": true,
    })
}

fn compare_rows(left: &Value, right: &Value) -> std::cmp::Ordering {
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
}
