use std::sync::Arc;

use serde_json::json;

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{
    coerce_truthy_arg, path_escapes_workspace_error, stringify_tool_arg, tool_error,
};
use crate::types::{ToolArguments, ToolExecutionResult};

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
            let write_content = format!(
                "{}{}{}",
                if leading_newline { "\n" } else { "" },
                content.as_str(),
                if trailing_newline { "\n" } else { "" }
            );
            match backend.write_text(&path, &write_content, append) {
                Ok(written) => ToolExecutionResult::success(
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
                ),
                Err(error) => workspace_backend_error(error),
            }
        }),
    );
    if let Some(schema) = crate::tools::schemas::schema_for("write_file") {
        spec.schema = schema;
    }
    spec
}
