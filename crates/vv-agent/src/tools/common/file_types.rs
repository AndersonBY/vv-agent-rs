use std::path::Path;

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
