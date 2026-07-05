pub mod edit;
pub mod file_io;
pub mod listing;

pub use edit::edit_file;
pub use file_io::{file_info, read_file, write_file};
pub use listing::list_files;

use std::io::ErrorKind;

use crate::tools::common::{path_escapes_workspace_error, tool_error};
use crate::types::ToolExecutionResult;

fn workspace_backend_error(error: std::io::Error) -> ToolExecutionResult {
    if error.kind() == ErrorKind::PermissionDenied
        && error.to_string().contains("Path escapes workspace")
    {
        return path_escapes_workspace_error(error.to_string());
    }
    tool_error(error.to_string())
}
