use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::process::{Command, Output};
use std::thread;
use std::time::Duration;

use regex::Regex;
use serde_json::{json, Value};

use crate::types::{ToolDirective, ToolExecutionResult, ToolResultStatus};

pub(crate) fn command_output_with_executable_busy_retry(
    command: &mut Command,
) -> io::Result<Output> {
    const MAX_ATTEMPTS: usize = 3;

    for attempt in 0..MAX_ATTEMPTS {
        match command.output() {
            Err(error)
                if error.kind() == io::ErrorKind::ExecutableFileBusy
                    && attempt + 1 < MAX_ATTEMPTS =>
            {
                thread::sleep(Duration::from_millis(10 * (attempt as u64 + 1)));
            }
            result => return result,
        }
    }

    unreachable!("command output retry loop always returns");
}

pub(crate) fn tool_error(message: impl Into<String>) -> ToolExecutionResult {
    tool_error_with_code(message, "")
}

pub(crate) fn tool_result(
    status: ToolResultStatus,
    content: Value,
    error_code: Option<&str>,
    directive: ToolDirective,
) -> ToolExecutionResult {
    let metadata = content
        .as_object()
        .map(|object| {
            object
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    ToolExecutionResult {
        tool_call_id: String::new(),
        content: content.to_string(),
        status,
        directive,
        error_code: error_code.map(str::to_string),
        metadata,
        image_url: None,
        image_path: None,
    }
}

pub(crate) fn tool_error_with_code(
    message: impl Into<String>,
    error_code: impl Into<String>,
) -> ToolExecutionResult {
    let error_code = error_code.into();
    ToolExecutionResult {
        tool_call_id: String::new(),
        content: json!({"ok": false, "error": message.into(), "error_code": error_code})
            .to_string(),
        status: ToolResultStatus::Error,
        directive: ToolDirective::Continue,
        error_code: if error_code.is_empty() {
            None
        } else {
            Some(error_code)
        },
        metadata: BTreeMap::new(),
        image_url: None,
        image_path: None,
    }
}

pub(crate) fn path_escapes_workspace_error(message: impl Into<String>) -> ToolExecutionResult {
    tool_error_with_code(message, "path_escapes_workspace")
}

pub(crate) fn coerce_bool(value: Option<&Value>, default: bool) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(value)) => match value.as_i64() {
            Some(0) => false,
            Some(1) => true,
            _ => default,
        },
        Some(Value::String(value)) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        },
        _ => default,
    }
}

pub(crate) fn parse_integer_arg(value: &Value) -> Result<i64, ()> {
    match value {
        Value::Number(number) => number.as_i64().ok_or(()),
        Value::String(text) => text.trim().parse::<i64>().map_err(|_| ()),
        _ => Err(()),
    }
}

pub(crate) fn coerce_python_text_arg(value: Option<&Value>, default: &str) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Bool(boolean)) => {
            if *boolean {
                "True".to_string()
            } else {
                "False".to_string()
            }
        }
        Some(Value::Null) => "None".to_string(),
        Some(other) => other.to_string(),
        None => default.to_string(),
    }
}

#[derive(Clone, Copy)]
pub(crate) struct GrepTextOptions {
    pub(crate) multiline: bool,
    pub(crate) before_context: usize,
    pub(crate) after_context: usize,
    pub(crate) show_line_numbers: bool,
}

pub(crate) struct GrepTextResult {
    pub(crate) rows: Vec<Value>,
    pub(crate) match_count: usize,
}

pub(crate) fn grep_text(
    relative_path: &str,
    text: &str,
    regex: &Regex,
    options: GrepTextOptions,
) -> GrepTextResult {
    if options.multiline {
        let rows = regex
            .find_iter(text)
            .map(|matched| {
                let line = text[..matched.start()]
                    .chars()
                    .filter(|ch| *ch == '\n')
                    .count()
                    + 1;
                json!({
                    "path": relative_path,
                    "line": line,
                    "text": matched.as_str(),
                    "is_match": true,
                })
            })
            .collect::<Vec<_>>();
        return GrepTextResult {
            match_count: rows.len(),
            rows,
        };
    }

    let lines = text.lines().collect::<Vec<_>>();
    let mut include_lines = BTreeMap::<usize, bool>::new();
    let mut match_count = 0usize;
    for (index, line) in lines.iter().enumerate() {
        let line_match_count = regex.find_iter(line).count();
        if line_match_count == 0 {
            continue;
        }
        match_count += line_match_count;
        let start = index.saturating_sub(options.before_context);
        let end = (index + options.after_context).min(lines.len().saturating_sub(1));
        for row_index in start..=end {
            include_lines.entry(row_index).or_insert(false);
        }
        include_lines.insert(index, true);
    }

    let rows = include_lines
        .into_iter()
        .map(|(index, is_match)| {
            let line_number = index + 1;
            let mut row = json!({
                "path": relative_path,
                "line": line_number,
                "text": lines[index],
                "is_match": is_match,
            });
            if !options.show_line_numbers {
                row.as_object_mut().expect("row object").remove("line");
            }
            row
        })
        .collect();
    GrepTextResult { rows, match_count }
}

pub(crate) fn is_hidden_path(path: &str) -> bool {
    path.split('/').any(|part| part.starts_with('.'))
}

const SUPPORTED_FILE_TYPES: &[&str] = &[
    "c",
    "cpp",
    "css",
    "dockerfile",
    "go",
    "html",
    "ini",
    "java",
    "js",
    "json",
    "log",
    "makefile",
    "md",
    "php",
    "py",
    "rb",
    "rust",
    "sh",
    "sql",
    "ts",
    "txt",
    "xml",
    "yaml",
];

pub(crate) fn supported_file_types_message() -> String {
    SUPPORTED_FILE_TYPES.join(", ")
}

pub(crate) fn is_supported_file_type(file_type: &str) -> bool {
    SUPPORTED_FILE_TYPES.contains(&file_type)
}

pub(crate) fn matches_file_type(path: &str, file_type: Option<&str>) -> bool {
    let Some(file_type) = file_type else {
        return !is_binary_path(path);
    };
    let lower = path.to_ascii_lowercase();
    let filename = Path::new(&lower)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let suffix = Path::new(&lower)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| format!(".{ext}"))
        .unwrap_or_default();
    match file_type {
        "py" => matches!(suffix.as_str(), ".py" | ".pyw" | ".pyi"),
        "js" => matches!(suffix.as_str(), ".js" | ".jsx" | ".mjs"),
        "ts" => matches!(suffix.as_str(), ".ts" | ".tsx"),
        "html" => matches!(suffix.as_str(), ".html" | ".htm" | ".xhtml"),
        "css" => matches!(suffix.as_str(), ".css" | ".scss" | ".sass" | ".less"),
        "java" => suffix == ".java",
        "c" => matches!(suffix.as_str(), ".c" | ".h"),
        "cpp" => matches!(
            suffix.as_str(),
            ".cpp" | ".cc" | ".cxx" | ".c++" | ".hpp" | ".hh" | ".hxx" | ".h++"
        ),
        "rust" => suffix == ".rs",
        "go" => suffix == ".go",
        "php" => matches!(suffix.as_str(), ".php" | ".php3" | ".php4" | ".php5"),
        "rb" => matches!(suffix.as_str(), ".rb" | ".rbx" | ".rhtml" | ".ruby"),
        "sh" => matches!(suffix.as_str(), ".sh" | ".bash" | ".zsh" | ".fish"),
        "sql" => suffix == ".sql",
        "json" => suffix == ".json",
        "xml" => matches!(suffix.as_str(), ".xml" | ".xsl" | ".xsd"),
        "yaml" => matches!(suffix.as_str(), ".yaml" | ".yml"),
        "md" => matches!(suffix.as_str(), ".md" | ".markdown" | ".mdown" | ".mkd"),
        "txt" => suffix == ".txt",
        "log" => suffix == ".log",
        "ini" => matches!(suffix.as_str(), ".ini" | ".cfg" | ".conf"),
        "dockerfile" => filename == "dockerfile",
        "makefile" => matches!(filename, "makefile" | "gnumakefile"),
        _ => false,
    }
}

fn is_binary_path(path: &str) -> bool {
    let suffix = Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| format!(".{}", ext.to_ascii_lowercase()))
        .unwrap_or_default();
    matches!(
        suffix.as_str(),
        ".png"
            | ".jpg"
            | ".jpeg"
            | ".gif"
            | ".webp"
            | ".bmp"
            | ".ico"
            | ".pdf"
            | ".zip"
            | ".tar"
            | ".gz"
            | ".bz2"
            | ".xz"
            | ".7z"
            | ".rar"
            | ".mp3"
            | ".wav"
            | ".mp4"
            | ".mov"
            | ".avi"
            | ".mkv"
            | ".exe"
            | ".dll"
            | ".so"
            | ".dylib"
            | ".bin"
    )
}

pub(crate) fn workspace_relative_path_or_absolute(workspace: &Path, path: &Path) -> String {
    if path == workspace {
        return ".".to_string();
    }
    path.strip_prefix(workspace)
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"))
}

pub(crate) fn replace_n(
    text: &str,
    old_str: &str,
    new_str: &str,
    max_replacements: usize,
) -> String {
    let mut remaining = text;
    let mut replaced = String::new();
    let mut count = 0;
    while count < max_replacements {
        let Some(index) = remaining.find(old_str) else {
            break;
        };
        replaced.push_str(&remaining[..index]);
        replaced.push_str(new_str);
        remaining = &remaining[index + old_str.len()..];
        count += 1;
    }
    replaced.push_str(remaining);
    replaced
}

pub(crate) fn collect_ignored_roots(files: &[String]) -> Vec<String> {
    let mut roots = files
        .iter()
        .filter_map(|path| path.split('/').next())
        .filter(|root| is_ignored_root(root))
        .map(str::to_string)
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    roots
}

pub(crate) fn is_ignored_root(root: &str) -> bool {
    matches!(
        root.to_ascii_lowercase().as_str(),
        ".venv"
            | "venv"
            | "node_modules"
            | ".git"
            | "__pycache__"
            | ".pytest_cache"
            | ".mypy_cache"
            | ".ruff_cache"
            | ".idea"
            | ".vscode"
            | "dist"
            | "build"
            | ".next"
            | ".nuxt"
            | ".cache"
            | "target"
            | "vendor"
    )
}
