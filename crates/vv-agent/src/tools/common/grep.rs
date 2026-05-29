use std::collections::BTreeMap;

use regex::Regex;
use serde_json::{json, Value};

#[derive(Clone, Copy)]
pub(crate) struct GrepTextOptions {
    pub(crate) multiline: bool,
    pub(crate) before_context: usize,
    pub(crate) after_context: usize,
    pub(crate) show_line_numbers: bool,
}

pub(crate) struct GrepTextResult {
    pub(crate) rows: Vec<Value>,
    pub(crate) match_count: usize,
}

pub(crate) fn grep_text(
    relative_path: &str,
    text: &str,
    regex: &Regex,
    options: GrepTextOptions,
) -> GrepTextResult {
    if options.multiline {
        let rows = regex
            .find_iter(text)
            .map(|matched| {
                let line = text[..matched.start()]
                    .chars()
                    .filter(|ch| *ch == '\n')
                    .count()
                    + 1;
                json!({
                    "path": relative_path,
                    "line": line,
                    "text": matched.as_str(),
                    "is_match": true,
                })
            })
            .collect::<Vec<_>>();
        return GrepTextResult {
            match_count: rows.len(),
            rows,
        };
    }

    let lines = text.lines().collect::<Vec<_>>();
    let mut include_lines = BTreeMap::<usize, bool>::new();
    let mut match_count = 0usize;
    for (index, line) in lines.iter().enumerate() {
        let line_match_count = regex.find_iter(line).count();
        if line_match_count == 0 {
            continue;
        }
        match_count += line_match_count;
        let start = index.saturating_sub(options.before_context);
        let end = (index + options.after_context).min(lines.len().saturating_sub(1));
        for row_index in start..=end {
            include_lines.entry(row_index).or_insert(false);
        }
        include_lines.insert(index, true);
    }

    let rows = include_lines
        .into_iter()
        .map(|(index, is_match)| {
            let line_number = index + 1;
            let mut row = json!({
                "path": relative_path,
                "line": line_number,
                "text": lines[index],
                "is_match": is_match,
            });
            if !options.show_line_numbers {
                row.as_object_mut().expect("row object").remove("line");
            }
            row
        })
        .collect();
    GrepTextResult { rows, match_count }
}
