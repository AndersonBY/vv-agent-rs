use std::path::PathBuf;

pub(in crate::tools::handlers::search) fn resolve_rg_executable() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(if cfg!(windows) { "rg.exe" } else { "rg" });
        if candidate.is_file() {
            return Some(candidate);
        }
        if cfg!(windows) {
            let candidate = directory.join("rg.cmd");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

pub(super) fn rg_file_type_globs(file_type: &str) -> Vec<String> {
    let tokens: &[&str] = match file_type {
        "py" => &[".py", ".pyw", ".pyi"],
        "js" => &[".js", ".jsx", ".mjs"],
        "ts" => &[".ts", ".tsx"],
        "html" => &[".html", ".htm", ".xhtml"],
        "css" => &[".css", ".scss", ".sass", ".less"],
        "java" => &[".java"],
        "c" => &[".c", ".h"],
        "cpp" => &[".cpp", ".cc", ".cxx", ".c++", ".hpp", ".hh", ".hxx", ".h++"],
        "rust" => &[".rs"],
        "go" => &[".go"],
        "php" => &[".php", ".php3", ".php4", ".php5"],
        "rb" => &[".rb", ".rbx", ".rhtml", ".ruby"],
        "sh" => &[".sh", ".bash", ".zsh", ".fish"],
        "sql" => &[".sql"],
        "json" => &[".json"],
        "xml" => &[".xml", ".xsl", ".xsd"],
        "yaml" => &[".yaml", ".yml"],
        "md" => &[".md", ".markdown", ".mdown", ".mkd"],
        "txt" => &[".txt"],
        "log" => &[".log"],
        "ini" => &[".ini", ".cfg", ".conf"],
        "dockerfile" => &["dockerfile"],
        "makefile" => &["makefile", "gnumakefile"],
        _ => &[],
    };
    tokens
        .iter()
        .map(|token| {
            if token.starts_with('.') {
                format!("**/*{token}")
            } else {
                format!("**/{token}")
            }
        })
        .collect()
}
