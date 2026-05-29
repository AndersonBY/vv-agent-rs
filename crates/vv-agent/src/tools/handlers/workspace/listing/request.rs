use crate::tools::common::{coerce_truthy_arg, parse_integer_arg, stringify_tool_arg};
use crate::types::ToolArguments;

#[derive(Debug, Clone)]
pub(super) struct ListFilesRequest {
    pub(super) path: String,
    pub(super) glob: String,
    pub(super) max_results: usize,
    pub(super) scan_limit: usize,
    pub(super) include_ignored: bool,
    pub(super) include_hidden: bool,
}

pub(super) struct ListFilesArgumentError {
    message: &'static str,
}

impl ListFilesArgumentError {
    pub(super) fn message(&self) -> &'static str {
        self.message
    }
}

impl ListFilesRequest {
    pub(super) fn from_arguments(
        arguments: &ToolArguments,
    ) -> Result<Self, ListFilesArgumentError> {
        let path = stringify_tool_arg(arguments.get("path"), ".");
        let glob = stringify_tool_arg(arguments.get("glob"), "**/*");
        let max_results = parse_max_results(arguments)?;
        let scan_limit = parse_scan_limit(arguments, max_results)?;
        Ok(Self {
            path,
            glob,
            max_results,
            scan_limit,
            include_ignored: coerce_truthy_arg(arguments.get("include_ignored"), false),
            include_hidden: coerce_truthy_arg(arguments.get("include_hidden"), false),
        })
    }

    pub(super) fn is_workspace_root(&self) -> bool {
        let normalized = self.path.trim();
        normalized.is_empty() || normalized.replace('\\', "/") == "."
    }
}

fn parse_max_results(arguments: &ToolArguments) -> Result<usize, ListFilesArgumentError> {
    match arguments.get("max_results") {
        Some(value) => match parse_integer_arg(value) {
            Ok(limit) => Ok(limit.clamp(1, 5_000) as usize),
            Err(_) => Err(integer_error()),
        },
        None => Ok(500),
    }
}

fn parse_scan_limit(
    arguments: &ToolArguments,
    max_results: usize,
) -> Result<usize, ListFilesArgumentError> {
    match arguments.get("scan_limit") {
        Some(value) => match parse_integer_arg(value) {
            Ok(limit) => Ok(limit.max(max_results as i64) as usize),
            Err(_) => Err(integer_error()),
        },
        None => Ok(50_000),
    }
}

fn integer_error() -> ListFilesArgumentError {
    ListFilesArgumentError {
        message: "`max_results` and `scan_limit` must be integers",
    }
}
