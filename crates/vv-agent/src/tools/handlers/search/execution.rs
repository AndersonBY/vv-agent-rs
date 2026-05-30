use crate::tools::base::ToolContext;
use crate::tools::common::path_escapes_workspace_error;
use crate::types::ToolExecutionResult;
use crate::workspace::LocalWorkspaceBackend;

use super::error::grep_error;
use super::fallback::workspace_grep_fallback;
use super::local_rg::{
    resolve_rg_executable, workspace_grep_local_rg, RgGrepResult, RgWorkspaceGrepRequest,
};
use super::request::WorkspaceGrepRequest;
use super::response::workspace_grep_success_response;

pub(super) fn execute_workspace_grep(
    context: &mut ToolContext,
    request: WorkspaceGrepRequest,
) -> ToolExecutionResult {
    if let Err(error) = context.resolve_workspace_path(&request.path) {
        return path_escapes_workspace_error(error);
    }

    let backend = context.effective_workspace_backend();
    let explicit_file_target = backend.is_file(&request.path);
    let rg_result = if explicit_file_target {
        None
    } else {
        backend
            .as_any()
            .downcast_ref::<LocalWorkspaceBackend>()
            .and_then(|_| {
                let rg_executable = resolve_rg_executable()?;
                workspace_grep_local_rg(RgWorkspaceGrepRequest {
                    context,
                    path: &request.path,
                    glob_pattern: &request.glob_pattern,
                    pattern: &request.pattern,
                    output_mode: &request.output_mode,
                    file_type: request.file_type.as_deref(),
                    case_insensitive: request.case_insensitive,
                    multiline: request.multiline,
                    before_context: request.before_context,
                    after_context: request.after_context,
                    include_hidden: request.include_hidden,
                    include_ignored: request.include_ignored,
                    rg_executable: &rg_executable,
                })
            })
    };

    let result = match rg_result {
        Some(result) => result,
        None => match workspace_grep_fallback(backend.as_ref(), &request, explicit_file_target) {
            Ok(result) => result,
            Err(error) => return grep_error(error),
        },
    };
    workspace_grep_success_response(&request, result)
}

pub(super) fn empty_grep_result() -> RgGrepResult {
    RgGrepResult {
        files_searched: 0,
        total_matches: 0,
        files_with_matches: Vec::new(),
        file_counts: Default::default(),
        rows: Vec::new(),
    }
}
