use regex::{Regex, RegexBuilder};

use crate::tools::common::{
    coerce_truthy_arg, is_supported_file_type, parse_integer_arg, stringify_tool_arg,
    supported_file_types_message,
};
use crate::types::ToolArguments;
use crate::workspace::normalized_glob_pattern;

pub(super) struct SearchFilesRequest {
    pub(super) pattern: String,
    pub(super) output_mode: String,
    pub(super) file_type: Option<String>,
    pub(super) path: String,
    pub(super) glob_pattern: String,
    pub(super) include_hidden: bool,
    pub(super) include_ignored: bool,
    pub(super) include_sensitive: bool,
    pub(super) literal: bool,
    pub(super) multiline: bool,
    pub(super) show_line_numbers: bool,
    pub(super) before_context: usize,
    pub(super) after_context: usize,
    pub(super) offset: usize,
    pub(super) head_limit: usize,
    pub(super) case_insensitive: bool,
    pub(super) regex: Regex,
}

pub(super) fn parse_search_files_request(
    arguments: &ToolArguments,
) -> Result<SearchFilesRequest, String> {
    let pattern = stringify_tool_arg(arguments.get("pattern"), "")
        .trim()
        .to_string();
    if pattern.is_empty() {
        return Err("Search pattern is required".to_string());
    }
    let output_mode = stringify_tool_arg(arguments.get("output_mode"), "files_with_matches");
    if !matches!(
        output_mode.as_str(),
        "content" | "files_with_matches" | "count"
    ) {
        return Err(format!(
            "Invalid `output_mode`: {output_mode}. Supported: content, count, files_with_matches"
        ));
    }
    let file_type = arguments
        .get("type")
        .map(|value| stringify_tool_arg(Some(value), ""))
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());
    if let Some(file_type) = &file_type {
        if !is_supported_file_type(file_type) {
            return Err(format!(
                "Unsupported file type: {file_type}. Supported types: {}",
                supported_file_types_message()
            ));
        }
    }
    let path = stringify_tool_arg(arguments.get("path"), ".");
    let glob = stringify_tool_arg(arguments.get("glob"), "**/*");
    let glob_pattern = normalized_glob_pattern(&glob);
    let include_hidden = coerce_truthy_arg(arguments.get("include_hidden"), false);
    let include_ignored = coerce_truthy_arg(arguments.get("include_ignored"), false);
    let include_sensitive = coerce_truthy_arg(arguments.get("include_sensitive"), false);
    let literal = coerce_truthy_arg(arguments.get("literal"), false);
    let multiline = coerce_truthy_arg(arguments.get("multiline"), false);
    let show_line_numbers = coerce_truthy_arg(arguments.get("n"), true);
    let context_lines = parse_optional_usize(arguments, "c", 0)?;
    let before_context = match context_lines {
        Some(value) => value,
        None => parse_optional_usize(arguments, "b", 0)?.unwrap_or(0),
    };
    let after_context = match context_lines {
        Some(value) => value,
        None => parse_optional_usize(arguments, "a", 0)?.unwrap_or(0),
    };
    let offset = parse_optional_usize(arguments, "offset", 0)?.unwrap_or(0);
    let head_limit = match arguments.get("head_limit") {
        Some(value) => parse_integer_arg(value)
            .map(|parsed| parsed.max(0) as usize)
            .map_err(|_| "`head_limit` must be an integer".to_string())?,
        None => 250,
    };
    let case_insensitive = if arguments.contains_key("case_sensitive") {
        !coerce_truthy_arg(arguments.get("case_sensitive"), false)
    } else {
        !pattern.chars().any(char::is_uppercase)
    };
    let regex_pattern = if literal {
        regex::escape(&pattern)
    } else {
        pattern.clone()
    };
    let regex = RegexBuilder::new(&regex_pattern)
        .case_insensitive(case_insensitive)
        .multi_line(multiline)
        .dot_matches_new_line(multiline)
        .build()
        .map_err(|error| format!("Invalid regular expression: {error}"))?;

    Ok(SearchFilesRequest {
        pattern,
        output_mode,
        file_type,
        path,
        glob_pattern,
        include_hidden,
        include_ignored,
        include_sensitive,
        literal,
        multiline,
        show_line_numbers,
        before_context,
        after_context,
        offset,
        head_limit,
        case_insensitive,
        regex,
    })
}

fn parse_optional_usize(
    arguments: &ToolArguments,
    name: &str,
    min_value: i64,
) -> Result<Option<usize>, String> {
    match arguments.get(name) {
        Some(value) => parse_integer_arg(value)
            .map(|parsed| Some(parsed.max(min_value) as usize))
            .map_err(|_| format!("`{name}` must be an integer")),
        None => Ok(None),
    }
}
