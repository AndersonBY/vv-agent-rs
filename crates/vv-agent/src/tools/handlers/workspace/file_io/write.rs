use std::sync::Arc;

use serde_json::{json, Value};

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{coerce_truthy_arg, path_escapes_workspace_error, stringify_tool_arg};
use crate::types::{Metadata, ToolArguments, ToolExecutionResult};

use super::super::edit::{
    baseline_issue, record_file_baseline, workspace_tool_error, workspace_tool_error_with_details,
    WRITE_FILE_ALLOWED_BASELINE_SOURCES, WRITE_FILE_BASELINE_SOURCE,
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
                return workspace_tool_error_with_details(
                    "`path` is required.",
                    "invalid_arguments",
                    Metadata::from([("missing_arguments".to_string(), json!(["path"]))]),
                );
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
            let mut known_full_before_write = !existing_file;
            if existing_file && !append {
                if let Some(issue) = baseline_issue(
                    context,
                    &path,
                    &before_raw,
                    WRITE_FILE_ALLOWED_BASELINE_SOURCES,
                    false,
                ) {
                    let message = if issue == "file_changed_since_read" {
                        "File changed since it was last read. Re-read it before overwriting."
                    } else {
                        "Read the full file with read_file before overwriting."
                    };
                    return workspace_tool_error(message, issue, &path);
                }
                known_full_before_write = true;
            } else if existing_file && append {
                known_full_before_write = baseline_issue(
                    context,
                    &path,
                    &before_raw,
                    WRITE_FILE_ALLOWED_BASELINE_SOURCES,
                    false,
                )
                .is_none();
            }
            let write_content = format!(
                "{}{}{}",
                if leading_newline { "\n" } else { "" },
                content.as_str(),
                if trailing_newline { "\n" } else { "" }
            );
            match backend.write_text(&path, &write_content, append) {
                Ok(written_bytes) => {
                    let updated_raw = match backend.read_bytes(&path) {
                        Ok(raw) => raw,
                        Err(error) => return workspace_backend_error(error),
                    };
                    record_file_baseline(
                        context,
                        &path,
                        &updated_raw,
                        append && existing_file && !known_full_before_write,
                        WRITE_FILE_BASELINE_SOURCE,
                    );
                    // Compatibility field: written_chars counts Unicode code points, not bytes.
                    let written_chars = write_content.chars().count();
                    let mut result = ToolExecutionResult::success(
                        "",
                        json!({
                            "ok": true,
                            "path": path,
                            "append": append,
                            "leading_newline": leading_newline,
                            "trailing_newline": trailing_newline,
                            "written_bytes": written_bytes,
                            "written_chars": written_chars,
                        })
                        .to_string(),
                    );
                    result.metadata = Metadata::from([
                        ("changed_files".to_string(), json!([path])),
                        (
                            "operation".to_string(),
                            Value::String("write_file".to_string()),
                        ),
                        ("append".to_string(), Value::Bool(append)),
                    ]);
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
