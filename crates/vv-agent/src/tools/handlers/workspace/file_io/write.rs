use std::sync::Arc;

use serde_json::json;

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{
    coerce_truthy_arg, path_escapes_workspace_error, stringify_tool_arg, tool_error,
};
use crate::types::{ToolArguments, ToolExecutionResult};

use super::super::edit::{
    baseline_issue, changed_file_metadata, detect_line_ending, record_file_baseline,
    workspace_tool_error,
};
use super::super::workspace_backend_error;

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
            let path = stringify_tool_arg(arguments.get("path"), "");
            if let Err(error) = context.resolve_workspace_path(&path) {
                return path_escapes_workspace_error(error);
            }
            let backend = context.effective_workspace_backend();
            let content = stringify_tool_arg(arguments.get("content"), "");
            let append = coerce_truthy_arg(arguments.get("append"), false);
            let leading_newline =
                append && coerce_truthy_arg(arguments.get("leading_newline"), false);
            let trailing_newline =
                append && coerce_truthy_arg(arguments.get("trailing_newline"), false);
            let existing_file = match backend.file_info(&path) {
                Ok(Some(info)) if info.is_file => true,
                Ok(_) => false,
                Err(error) => return workspace_backend_error(error),
            };
            let before_raw = if existing_file {
                match backend.read_bytes(&path) {
                    Ok(raw) => raw,
                    Err(error) => return workspace_backend_error(error),
                }
            } else {
                Vec::new()
            };
            if existing_file && !append {
                if let Some(issue) = baseline_issue(context, &path, &before_raw) {
                    let message = if issue == "file_changed_since_read" {
                        "File changed since it was last read. Re-read it before overwriting."
                    } else {
                        "Read the full file with read_file before overwriting."
                    };
                    return workspace_tool_error(message, issue, &path);
                }
            }
            let before_text = String::from_utf8_lossy(&before_raw).to_string();
            let write_content = format!(
                "{}{}{}",
                if leading_newline { "\n" } else { "" },
                content.as_str(),
                if trailing_newline { "\n" } else { "" }
            );
            match backend.write_text(&path, &write_content, append) {
                Ok(written) => {
                    let updated_raw = match backend.read_bytes(&path) {
                        Ok(raw) => raw,
                        Err(error) => return workspace_backend_error(error),
                    };
                    let updated_text = String::from_utf8_lossy(&updated_raw).to_string();
                    record_file_baseline(context, &path, &updated_raw, false);
                    let line_ending = detect_line_ending(&updated_text);
                    let mut result = ToolExecutionResult::success(
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
                    );
                    result.metadata = changed_file_metadata(
                        &path,
                        &before_text,
                        &updated_text,
                        "write_file",
                        line_ending,
                    );
                    result
                }
                Err(error) => workspace_backend_error(error),
            }
        }),
    );
    if let Some(schema) = crate::tools::schemas::schema_for("write_file") {
        spec.schema = schema;
    }
    spec
}
