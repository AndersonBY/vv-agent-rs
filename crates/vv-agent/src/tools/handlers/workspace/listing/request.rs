use crate::tools::common::{coerce_truthy_arg, parse_integer_arg, stringify_tool_arg};
use crate::types::ToolArguments;

#[derive(Debug, Clone)]
pub(super) struct FindFilesRequest {
    pub(super) path: String,
    pub(super) glob: String,
    pub(super) max_results: usize,
    pub(super) scan_limit: usize,
    pub(super) offset: usize,
    pub(super) sort: String,
    pub(super) include_ignored: bool,
    pub(super) include_hidden: bool,
    pub(super) include_sensitive: bool,
}

pub(super) struct FindFilesArgumentError {
    message: &'static str,
}

impl FindFilesArgumentError {
    pub(super) fn message(&self) -> &'static str {
        self.message
    }
}

impl FindFilesRequest {
    pub(super) fn from_arguments(
        arguments: &ToolArguments,
    ) -> Result<Self, FindFilesArgumentError> {
        if arguments.contains_key("pattern") {
            return Err(FindFilesArgumentError {
                message: "`glob` is required for file patterns; `pattern` is not supported",
            });
        }
        let path = stringify_tool_arg(arguments.get("path"), ".");
        let glob = stringify_tool_arg(arguments.get("glob"), "**/*");
        let max_results = parse_max_results(arguments)?;
        let scan_limit = parse_scan_limit(arguments, max_results)?;
        let offset = parse_offset(arguments)?;
        let sort = stringify_tool_arg(arguments.get("sort"), "modified_desc");
        if !matches!(sort.as_str(), "modified_desc" | "path_asc") {
            return Err(FindFilesArgumentError {
                message: "`sort` must be modified_desc or path_asc",
            });
        }
        Ok(Self {
            path,
            glob,
            max_results,
            scan_limit,
            offset,
            sort,
            include_ignored: coerce_truthy_arg(arguments.get("include_ignored"), false),
            include_hidden: coerce_truthy_arg(arguments.get("include_hidden"), false),
            include_sensitive: coerce_truthy_arg(arguments.get("include_sensitive"), false),
        })
    }

    pub(super) fn is_workspace_root(&self) -> bool {
        let normalized = self.path.trim();
        normalized.is_empty() || normalized.replace('\\', "/") == "."
    }
}

fn parse_max_results(arguments: &ToolArguments) -> Result<usize, FindFilesArgumentError> {
    match arguments.get("max_results") {
        Some(value) => match parse_integer_arg(value) {
            Ok(limit) => Ok(limit.clamp(1, 5_000) as usize),
            Err(_) => Err(integer_error()),
        },
        None => Ok(100),
    }
}

fn parse_scan_limit(
    arguments: &ToolArguments,
    max_results: usize,
) -> Result<usize, FindFilesArgumentError> {
    match arguments.get("scan_limit") {
        Some(value) => match parse_integer_arg(value) {
            Ok(limit) => Ok(limit.max(max_results as i64) as usize),
            Err(_) => Err(integer_error()),
        },
        None => Ok(50_000),
    }
}

fn parse_offset(arguments: &ToolArguments) -> Result<usize, FindFilesArgumentError> {
    match arguments.get("offset") {
        Some(value) => match parse_integer_arg(value) {
            Ok(offset) => Ok(offset.max(0) as usize),
            Err(_) => Err(integer_error()),
        },
        None => Ok(0),
    }
}

fn integer_error() -> FindFilesArgumentError {
    FindFilesArgumentError {
        message: "`max_results` and `scan_limit` must be integers",
    }
}
