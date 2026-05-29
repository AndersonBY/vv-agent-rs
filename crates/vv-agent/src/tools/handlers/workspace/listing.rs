mod fallback;
mod local_rg;
mod request;
mod response;
mod types;

use std::sync::Arc;

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{path_escapes_workspace_error, tool_error};
use crate::types::{ToolArguments, ToolExecutionResult};
use crate::workspace::LocalWorkspaceBackend;

use request::ListFilesRequest;

use super::workspace_backend_error;

pub fn list_files(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let spec = list_files_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn list_files_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "list_files",
        "List files in the current workspace.",
        Arc::new(handle_list_files),
    );
    if let Some(schema) = crate::tools::schemas::schema_for("list_files") {
        spec.schema = schema;
    }
    spec
}

fn handle_list_files(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let request = match ListFilesRequest::from_arguments(arguments) {
        Ok(request) => request,
        Err(error) => return tool_error(error.message()),
    };
    let resolved_path = match context.resolve_workspace_path(&request.path) {
        Ok(path) => path,
        Err(error) => return path_escapes_workspace_error(error),
    };

    let backend = context.effective_workspace_backend();
    let root_listing = request.is_workspace_root();
    let is_local_backend = backend.as_any().is::<LocalWorkspaceBackend>();
    let rg_outcome = backend
        .as_any()
        .downcast_ref::<LocalWorkspaceBackend>()
        .and_then(|_| {
            local_rg::list_files_with_rg(context, &resolved_path, root_listing, &request)
        });

    let outcome = if let Some(outcome) = rg_outcome {
        Ok(outcome)
    } else {
        fallback::list_files_with_backend(&backend, &request, is_local_backend, root_listing)
    };

    match outcome {
        Ok(outcome) => response::render_list_files(outcome, &request),
        Err(error) => workspace_backend_error(error),
    }
}
