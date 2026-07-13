use serde_json::{json, Value};

use crate::types::{ToolDirective, ToolExecutionResult, ToolResultStatus};

use super::format::{
    cap_file_counts, cap_file_paths, cap_match_rows, render_grep_content, truncate_result_text,
    MAX_STRUCTURED_CHARS, MAX_STRUCTURED_ITEMS,
};
use super::local_rg::RgGrepResult;
use super::request::SearchFilesRequest;

pub(super) fn search_files_success_response(
    request: &SearchFilesRequest,
    mut result: RgGrepResult,
) -> ToolExecutionResult {
    result.files_with_matches.sort();
    let files_with_match_count = result.files_with_matches.len();
    let total_result_items = match request.output_mode.as_str() {
        "files_with_matches" => result.files_with_matches.len(),
        "count" => result.file_counts.len(),
        _ => result.rows.len(),
    };
    let head_limited;
    match request.output_mode.as_str() {
        "files_with_matches" => {
            let (start, end, limited) = slice_bounds(
                result.files_with_matches.len(),
                request.offset,
                request.head_limit,
            );
            head_limited = limited;
            result.files_with_matches = result.files_with_matches[start..end].to_vec();
        }
        "count" => {
            let count_items = result.file_counts.into_iter().collect::<Vec<_>>();
            let (start, end, limited) =
                slice_bounds(count_items.len(), request.offset, request.head_limit);
            head_limited = limited;
            result.file_counts = count_items[start..end].iter().cloned().collect();
        }
        _ => {
            let (start, end, limited) =
                slice_bounds(result.rows.len(), request.offset, request.head_limit);
            head_limited = limited;
            result.rows = result.rows[start..end].to_vec();
        }
    }
    let structured_capped;
    match request.output_mode.as_str() {
        "files_with_matches" => {
            let (capped_files, capped) = cap_file_paths(result.files_with_matches);
            result.files_with_matches = capped_files;
            structured_capped = capped;
        }
        "count" => {
            let (capped_counts, capped) = cap_file_counts(result.file_counts);
            result.file_counts = capped_counts;
            structured_capped = capped;
        }
        _ => {
            let (capped_rows, capped) = cap_match_rows(result.rows);
            result.rows = capped_rows;
            structured_capped = capped;
        }
    }
    let structured_truncated = head_limited || structured_capped;

    let summary = json!({
        "files_searched": result.files_searched,
        "files_with_matches": files_with_match_count,
        "total_matches": result.total_matches,
    });
    let mut payload = json!({
        "summary": summary,
        "pattern": request.pattern.clone(),
        "path": request.path.clone(),
        "glob": request.glob_pattern.clone(),
        "type": request.file_type.clone(),
        "output_mode": request.output_mode.clone(),
        "literal": request.literal,
        "offset": request.offset,
        "head_limit": request.head_limit,
        "head_limited": head_limited,
        "total_result_items": total_result_items,
        "returned_count": match request.output_mode.as_str() {
            "files_with_matches" => result.files_with_matches.len(),
            "count" => result.file_counts.len(),
            _ => result.rows.len(),
        },
        "content_truncated": false,
        "structured_truncated": structured_truncated,
        "truncated": structured_truncated,
    });
    if result.sensitive_files_omitted > 0 {
        payload["sensitive_files_omitted"] = json!(result.sensitive_files_omitted);
    }
    if structured_capped {
        payload["structured_item_limit"] = json!(MAX_STRUCTURED_ITEMS);
        payload["structured_char_limit"] = json!(MAX_STRUCTURED_CHARS);
    }
    match request.output_mode.as_str() {
        "files_with_matches" => payload["files"] = json!(result.files_with_matches),
        "count" => payload["file_counts"] = json!(result.file_counts),
        _ => payload["matches"] = Value::Array(result.rows),
    }
    let content = render_grep_content(
        &request.output_mode,
        &request.pattern,
        &payload,
        request.show_line_numbers,
        structured_truncated,
    );
    let (content, content_truncated) =
        truncate_result_text(content, result.total_matches, files_with_match_count);
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
}

fn slice_bounds(total: usize, offset: usize, head_limit: usize) -> (usize, usize, bool) {
    if offset >= total {
        return (total, total, false);
    }
    if head_limit == 0 {
        return (offset, total, false);
    }
    let end = offset.saturating_add(head_limit).min(total);
    (offset, end, end < total)
}
