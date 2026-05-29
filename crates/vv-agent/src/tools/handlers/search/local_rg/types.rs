use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::tools::base::ToolContext;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::tools::handlers::search) struct RgGrepResult {
    pub files_searched: usize,
    pub total_matches: usize,
    pub files_with_matches: Vec<String>,
    pub file_counts: BTreeMap<String, usize>,
    pub rows: Vec<Value>,
}

pub(in crate::tools::handlers::search) struct RgWorkspaceGrepRequest<'a> {
    pub context: &'a ToolContext,
    pub path: &'a str,
    pub glob_pattern: &'a str,
    pub pattern: &'a str,
    pub output_mode: &'a str,
    pub file_type: Option<&'a str>,
    pub case_insensitive: bool,
    pub multiline: bool,
    pub before_context: usize,
    pub after_context: usize,
    pub include_hidden: bool,
    pub include_ignored: bool,
    pub rg_executable: &'a Path,
}
