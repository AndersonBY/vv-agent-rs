use serde_json::{json, Value};

use crate::types::ToolExecutionResult;

use super::request::FindFilesRequest;
use super::types::FindFilesOutcome;

pub(super) fn render_find_files(
    outcome: FindFilesOutcome,
    request: &FindFilesRequest,
) -> ToolExecutionResult {
    let returned_count = outcome.files.len();
    let mut payload = json!({
        "files": outcome.files,
        "count": outcome.count,
        "returned_count": returned_count,
        "truncated": outcome.truncated,
        "max_results": request.max_results,
        "offset": request.offset,
        "sort": outcome.sort,
    });
    let consumed = request.offset.saturating_add(returned_count);
    if outcome.count > consumed {
        payload["remaining_count"] = Value::Number((outcome.count - consumed).into());
    }
    if outcome.scan_limited {
        payload["count_is_estimate"] = Value::Bool(true);
        payload["scan_limit"] = Value::Number(request.scan_limit.into());
        payload["message"] = Value::String(
            "Listing stopped early due to scan limit. Narrow `path`/`glob` or increase `scan_limit` for more complete results."
                .to_string(),
        );
    }
    if !outcome.ignored_roots.is_empty() {
        payload["ignored_roots"] = Value::Array(
            outcome
                .ignored_roots
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
    if outcome.sensitive_files_omitted > 0 {
        payload["sensitive_files_omitted"] = Value::Number(outcome.sensitive_files_omitted.into());
    }
    ToolExecutionResult::success("", payload.to_string())
}
