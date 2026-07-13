use std::collections::BTreeMap;

use serde_json::Value;

pub(super) const MAX_STRUCTURED_ITEMS: usize = 200;
pub(super) const MAX_STRUCTURED_CHARS: usize = 20_000;

const MAX_RESULT_LINES: usize = 500;
const MAX_RESULT_CHARS: usize = 30_000;

pub(super) fn render_grep_content(
    output_mode: &str,
    pattern: &str,
    payload: &Value,
    show_line_numbers: bool,
    head_limited: bool,
) -> String {
    let summary = &payload["summary"];
    let total_matches = summary["total_matches"].as_u64().unwrap_or_default();
    let files_with_matches = summary["files_with_matches"].as_u64().unwrap_or_default();
    let pattern = python_string_repr(pattern);
    match output_mode {
        "files_with_matches" => {
            let files = payload["files"].as_array().cloned().unwrap_or_default();
            let mut lines = vec![format!(
                "Found {files_with_matches} files matching pattern {pattern}"
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
            let mut lines = vec![format!("Match counts for pattern {pattern}")];
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
                "Found {total_matches} matches in {files_with_matches} files for pattern {pattern}"
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

fn python_string_repr(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut rendered = String::with_capacity(value.len() + 2);
    rendered.push(quote);
    for character in value.chars() {
        match character {
            '\\' => rendered.push_str("\\\\"),
            '\t' => rendered.push_str("\\t"),
            '\n' => rendered.push_str("\\n"),
            '\r' => rendered.push_str("\\r"),
            character if character == quote => {
                rendered.push('\\');
                rendered.push(character);
            }
            character if character.is_control() => {
                let codepoint = character as u32;
                if codepoint <= 0xff {
                    rendered.push_str(&format!("\\x{codepoint:02x}"));
                } else if codepoint <= 0xffff {
                    rendered.push_str(&format!("\\u{codepoint:04x}"));
                } else {
                    rendered.push_str(&format!("\\U{codepoint:08x}"));
                }
            }
            character => rendered.push(character),
        }
    }
    rendered.push(quote);
    rendered
}

pub(super) fn truncate_result_text(
    result_text: String,
    total_matches: usize,
    files_with_matches: usize,
) -> (String, bool) {
    let line_count = result_text.lines().count();
    let result_char_count = result_text.chars().count();
    if line_count <= MAX_RESULT_LINES && result_char_count <= MAX_RESULT_CHARS {
        return (result_text, false);
    }

    let truncated = if result_char_count > MAX_RESULT_CHARS {
        let end = result_text
            .char_indices()
            .nth(MAX_RESULT_CHARS)
            .map_or(result_text.len(), |(index, _)| index);
        let candidate = &result_text[..end];
        match candidate.rfind('\n') {
            Some(last_newline)
                if candidate[..last_newline].chars().count() > MAX_RESULT_CHARS * 4 / 5 =>
            {
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
        truncated.chars().count()
    );
    (format!("{truncated}{truncated_info}"), true)
}

pub(super) fn cap_file_paths(items: Vec<String>) -> (Vec<String>, bool) {
    cap_structured_items(items, |path| path.chars().count() + 4)
}

pub(super) fn cap_file_counts(
    file_counts: BTreeMap<String, usize>,
) -> (BTreeMap<String, usize>, bool) {
    let count_items = file_counts.into_iter().collect::<Vec<_>>();
    let (capped_items, capped) = cap_structured_items(count_items, |(path, count)| {
        path.chars().count() + count.to_string().len() + 8
    });
    (capped_items.into_iter().collect(), capped)
}

pub(super) fn cap_match_rows(rows: Vec<Value>) -> (Vec<Value>, bool) {
    cap_structured_items(rows, |row| {
        row["path"].as_str().map_or(0, |path| path.chars().count())
            + row["line"]
                .as_u64()
                .map_or(0, |line| line.to_string().len())
            + row["text"].as_str().map_or(0, |text| text.chars().count())
            + 32
    })
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
