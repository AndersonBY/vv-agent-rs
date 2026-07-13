mod fallback;
mod local_rg;
mod request;
mod response;
mod types;

use std::sync::Arc;

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{path_escapes_workspace_error, tool_error_with_code};
use crate::types::{ToolArguments, ToolExecutionResult};
use crate::workspace::LocalWorkspaceBackend;

use request::FindFilesRequest;

use super::edit::workspace_tool_error;
use super::workspace_backend_error;

pub fn find_files(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let spec = find_files_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn find_files_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "find_files",
        "Find files in the current workspace.",
        Arc::new(handle_find_files),
    );
    if let Some(schema) = crate::tools::schemas::schema_for("find_files") {
        spec.schema = schema;
    }
    spec
}

fn handle_find_files(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let request = match FindFilesRequest::from_arguments(arguments) {
        Ok(request) => request,
        Err(error) => return tool_error_with_code(error.message(), "invalid_arguments"),
    };
    let resolved_path = match context.resolve_workspace_path(&request.path) {
        Ok(path) => path,
        Err(error) => return path_escapes_workspace_error(error),
    };

    let backend = context.effective_workspace_backend();
    let root_listing = request.is_workspace_root();
    let is_local_backend = backend.as_any().is::<LocalWorkspaceBackend>();
    if is_local_backend && !resolved_path.exists() {
        return workspace_tool_error(
            format!("path not found: {}", request.path),
            "path_not_found",
            &request.path,
        );
    }
    if is_local_backend && !resolved_path.is_dir() {
        return workspace_tool_error(
            format!("not a directory: {}", request.path),
            "not_a_directory",
            &request.path,
        );
    }
    let rg_outcome = backend
        .as_any()
        .downcast_ref::<LocalWorkspaceBackend>()
        .and_then(|_| {
            local_rg::find_files_with_rg(context, &resolved_path, root_listing, &request)
        });

    let outcome = if let Some(outcome) = rg_outcome {
        Ok(outcome)
    } else {
        fallback::find_files_with_backend(&backend, &request, is_local_backend, root_listing)
    };

    match outcome {
        Ok(outcome) => response::render_find_files(outcome, &request),
        Err(error) => workspace_backend_error(error),
    }
}
