use std::sync::Arc;

use serde_json::json;

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{
    coerce_truthy_arg, parse_integer_arg, path_escapes_workspace_error, replace_n,
    stringify_tool_arg, tool_error,
};
use crate::types::{ToolArguments, ToolExecutionResult};

use super::workspace_backend_error;

pub fn file_str_replace(
    context: &mut ToolContext,
    arguments: &ToolArguments,
) -> ToolExecutionResult {
    let spec = file_str_replace_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn file_str_replace_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "file_str_replace",
        "Replace text in a workspace file.",
        Arc::new(|context, arguments| {
            if !arguments.contains_key("path") {
                return tool_error("missing required argument: path");
            }
            let path = stringify_tool_arg(arguments.get("path"), "");
            if let Err(error) = context.resolve_workspace_path(&path) {
                return path_escapes_workspace_error(error);
            }
            let backend = context.effective_workspace_backend();
            match backend.file_info(&path) {
                Ok(Some(info)) if info.is_file => {}
                Ok(_) => return tool_error(format!("file not found: {path}")),
                Err(error) => return workspace_backend_error(error),
            }
            let old_str = stringify_tool_arg(arguments.get("old_str"), "");
            if old_str.is_empty() {
                return tool_error("`old_str` cannot be empty");
            }
            let new_str = stringify_tool_arg(arguments.get("new_str"), "");
            let replace_all = coerce_truthy_arg(arguments.get("replace_all"), false);
            let max_replacements = match arguments.get("max_replacements") {
                Some(value) => match parse_integer_arg(value) {
                    Ok(limit) => limit.max(1) as usize,
                    Err(_) => return tool_error("`max_replacements` must be an integer"),
                },
                None => 1,
            };
            match backend.read_text(&path) {
                Ok(text) => {
                    let occurrence_count = text.matches(&old_str).count();
                    if occurrence_count == 0 {
                        return tool_error("`old_str` not found in file");
                    }
                    let replaced_count = if replace_all {
                        occurrence_count
                    } else {
                        occurrence_count.min(max_replacements)
                    };
                    let replaced_text = if replace_all {
                        text.replace(&old_str, &new_str)
                    } else {
                        replace_n(&text, &old_str, &new_str, max_replacements)
                    };
                    match backend.write_text(&path, &replaced_text, false) {
                        Ok(_) => crate::types::ToolExecutionResult::success(
                            "",
                            json!({
                                "ok": true,
                                "path": path,
                                "replaced_count": replaced_count,
                            })
                            .to_string(),
                        ),
                        Err(error) => workspace_backend_error(error),
                    }
                }
                Err(error) => workspace_backend_error(error),
            }
        }),
    );
    if let Some(schema) = crate::tools::schemas::schema_for("file_str_replace") {
        spec.schema = schema;
    }
    spec
}
