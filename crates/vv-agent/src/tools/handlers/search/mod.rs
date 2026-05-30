mod error;
mod execution;
mod fallback;
mod format;
mod local_rg;
mod request;
mod response;

use std::sync::Arc;

use crate::tools::base::{ToolContext, ToolSpec};
use crate::types::{ToolArguments, ToolExecutionResult};

use error::grep_error;
use execution::execute_workspace_grep;
use request::parse_workspace_grep_request;

pub fn workspace_grep(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let spec = workspace_grep_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn workspace_grep_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "workspace_grep",
        "Search workspace files with grep-style semantics.",
        Arc::new(|context, arguments| {
            let request = match parse_workspace_grep_request(arguments) {
                Ok(request) => request,
                Err(error) => return grep_error(error),
            };
            execute_workspace_grep(context, request)
        }),
    );
    if let Some(schema) = crate::tools::schemas::schema_for("workspace_grep") {
        spec.schema = schema;
    }
    spec
}
