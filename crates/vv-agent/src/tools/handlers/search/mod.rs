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
use execution::execute_search_files;
use request::parse_search_files_request;

pub fn search_files(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let spec = search_files_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn search_files_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "search_files",
        "Search workspace file contents with grep-style semantics.",
        Arc::new(|context, arguments| {
            let request = match parse_search_files_request(arguments) {
                Ok(request) => request,
                Err(error) => return grep_error(error),
            };
            execute_search_files(context, request)
        }),
    );
    if let Some(schema) = crate::tools::schemas::schema_for("search_files") {
        spec.schema = schema;
    }
    spec
}
