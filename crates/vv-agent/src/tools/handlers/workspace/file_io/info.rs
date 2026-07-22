use std::sync::Arc;

use serde_json::{json, Value};

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{path_escapes_workspace_error, string_arg};
use crate::types::{Metadata, ToolArguments, ToolExecutionResult};

use super::super::edit::{workspace_tool_error, workspace_tool_error_with_details};

pub fn file_info(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let spec = file_info_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn file_info_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "file_info",
        "Return metadata for a workspace path.",
        Arc::new(|context, arguments| {
            if !arguments.contains_key("path") {
                return workspace_tool_error_with_details(
                    "`path` is required.",
                    "invalid_arguments",
                    Metadata::from([("missing_arguments".to_string(), json!(["path"]))]),
                );
            }
            let path = string_arg(arguments.get("path"), "");
            if let Err(error) = context.resolve_workspace_path(&path) {
                return path_escapes_workspace_error(error);
            }
            let backend = context.effective_workspace_backend();
            match backend.file_info(&path) {
                Ok(Some(info)) => {
                    let mut payload = json!({
                        "path": info.path,
                        "exists": true,
                        "is_file": info.is_file,
                        "is_dir": info.is_dir,
                        "size": info.size,
                        "modified_at": info.modified_at,
                    });
                    if info.is_file {
                        payload["suffix"] = Value::String(info.suffix);
                    }
                    ToolExecutionResult::success("", payload.to_string())
                }
                Ok(None) => {
                    workspace_tool_error(format!("path not found: {path}"), "path_not_found", &path)
                }
                Err(error) => {
                    workspace_tool_error(error.to_string(), "workspace_backend_error", &path)
                }
            }
        }),
    );
    if let Some(schema) = crate::tools::schemas::schema_for("file_info") {
        spec.schema = schema;
    }
    spec
}
