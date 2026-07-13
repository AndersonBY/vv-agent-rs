use std::any::Any;
use std::sync::Arc;

use regex::Regex;

use super::{normalize_workspace_path, FileInfo, WorkspaceBackend};

pub const INVALID_EXCLUDE_FILES_PATTERN_CODE: &str = "invalid_exclude_files_pattern";
pub const INVALID_EXCLUDE_FILES_PATTERN_MESSAGE: &str =
    "exclude_files_pattern must be a valid portable regular expression";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortableRegexError;

impl PortableRegexError {
    pub fn code(&self) -> &'static str {
        INVALID_EXCLUDE_FILES_PATTERN_CODE
    }

    pub fn message(&self) -> &'static str {
        INVALID_EXCLUDE_FILES_PATTERN_MESSAGE
    }
}

impl std::fmt::Display for PortableRegexError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(INVALID_EXCLUDE_FILES_PATTERN_MESSAGE)
    }
}

impl std::error::Error for PortableRegexError {}

#[derive(Clone)]
pub struct DiscoveryFilteredWorkspaceBackend {
    inner: Arc<dyn WorkspaceBackend>,
    exclude_pattern: Regex,
    pattern_source: String,
}

impl std::fmt::Debug for DiscoveryFilteredWorkspaceBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DiscoveryFilteredWorkspaceBackend")
            .field("exclude_pattern", &self.pattern_source)
            .finish_non_exhaustive()
    }
}

impl DiscoveryFilteredWorkspaceBackend {
    pub fn new(
        inner: Arc<dyn WorkspaceBackend>,
        pattern: impl Into<String>,
    ) -> Result<Self, PortableRegexError> {
        let pattern_source = pattern.into();
        let exclude_pattern = compile_portable_regex(&pattern_source)?;
        Ok(Self {
            inner,
            exclude_pattern,
            pattern_source,
        })
    }

    pub fn inner(&self) -> &Arc<dyn WorkspaceBackend> {
        &self.inner
    }

    pub fn pattern(&self) -> &str {
        &self.pattern_source
    }
}

pub fn validate_portable_exclude_pattern(pattern: &str) -> Result<(), PortableRegexError> {
    compile_portable_regex(pattern).map(|_| ())
}

fn compile_portable_regex(pattern: &str) -> Result<Regex, PortableRegexError> {
    validate_portable_regex_syntax(pattern)?;
    Regex::new(&with_ascii_portable_classes(pattern)).map_err(|_| PortableRegexError)
}

fn with_ascii_portable_classes(pattern: &str) -> String {
    let mut normalized = String::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            normalized.push(character);
            continue;
        }
        let Some(escaped) = chars.next() else {
            normalized.push('\\');
            break;
        };
        match escaped {
            'w' => normalized.push_str("[A-Za-z0-9_]"),
            'W' => normalized.push_str("[^A-Za-z0-9_]"),
            's' => normalized.push_str(r"[\t\n\r\x0B\x0C ]"),
            'S' => normalized.push_str(r"[^\t\n\r\x0B\x0C ]"),
            _ => {
                normalized.push('\\');
                normalized.push(escaped);
            }
        }
    }
    normalized
}

fn validate_portable_regex_syntax(pattern: &str) -> Result<(), PortableRegexError> {
    let bytes = pattern.as_bytes();
    let mut index = 0usize;
    let mut in_character_class = false;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => {
                let Some(escaped) = bytes.get(index + 1).copied() else {
                    return Err(PortableRegexError);
                };
                if escaped.is_ascii_alphanumeric()
                    && !matches!(
                        escaped,
                        b'A' | b'a'
                            | b'b'
                            | b'B'
                            | b'd'
                            | b'D'
                            | b'f'
                            | b'n'
                            | b'r'
                            | b's'
                            | b'S'
                            | b't'
                            | b'v'
                            | b'w'
                            | b'W'
                            | b'x'
                    )
                {
                    return Err(PortableRegexError);
                }
                if escaped == b'x' && bytes.get(index + 2) == Some(&b'{') {
                    return Err(PortableRegexError);
                }
                if in_character_class && matches!(escaped, b'A' | b'b' | b'B') {
                    return Err(PortableRegexError);
                }
                index += 2;
                continue;
            }
            b'[' if !in_character_class => in_character_class = true,
            b'[' if in_character_class => return Err(PortableRegexError),
            b']' if in_character_class => in_character_class = false,
            b'&' | b'-' | b'~'
                if in_character_class && bytes.get(index + 1) == bytes.get(index) =>
            {
                return Err(PortableRegexError);
            }
            b'(' if !in_character_class
                && bytes.get(index + 1) == Some(&b'?')
                && bytes.get(index + 2) != Some(&b':') =>
            {
                return Err(PortableRegexError);
            }
            b'{' if !in_character_class && bytes.get(index + 1) == Some(&b',') => {
                return Err(PortableRegexError);
            }
            b'*' | b'+' | b'?' | b'}'
                if !in_character_class && bytes.get(index + 1) == Some(&b'+') =>
            {
                return Err(PortableRegexError);
            }
            b'*' | b'+' | b'?'
                if !in_character_class && matches!(bytes.get(index + 1), Some(b'*' | b'+')) =>
            {
                return Err(PortableRegexError);
            }
            _ => {}
        }
        index += 1;
    }
    Ok(())
}

impl WorkspaceBackend for DiscoveryFilteredWorkspaceBackend {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn list_files(&self, base: &str, glob: &str) -> std::io::Result<Vec<String>> {
        Ok(self
            .inner
            .list_files(base, glob)?
            .into_iter()
            .filter(|path| {
                let normalized = normalize_workspace_path(path);
                !self.exclude_pattern.is_match(&normalized)
            })
            .collect())
    }

    fn read_text(&self, path: &str) -> std::io::Result<String> {
        self.inner.read_text(path)
    }

    fn read_bytes(&self, path: &str) -> std::io::Result<Vec<u8>> {
        self.inner.read_bytes(path)
    }

    fn write_text(&self, path: &str, content: &str, append: bool) -> std::io::Result<usize> {
        self.inner.write_text(path, content, append)
    }

    fn file_info(&self, path: &str) -> std::io::Result<Option<FileInfo>> {
        self.inner.file_info(path)
    }

    fn exists(&self, path: &str) -> bool {
        self.inner.exists(path)
    }

    fn is_file(&self, path: &str) -> bool {
        self.inner.is_file(path)
    }

    fn mkdir(&self, path: &str) -> std::io::Result<()> {
        self.inner.mkdir(path)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        validate_portable_exclude_pattern, DiscoveryFilteredWorkspaceBackend,
        INVALID_EXCLUDE_FILES_PATTERN_CODE, INVALID_EXCLUDE_FILES_PATTERN_MESSAGE,
    };
    use crate::workspace::{MemoryWorkspaceBackend, WorkspaceBackend};

    #[test]
    fn portable_regex_contract_accepts_non_capturing_alternation() {
        validate_portable_exclude_pattern(r"^(?:generated|logs)/").expect("portable pattern");
    }

    #[test]
    fn portable_regex_contract_rejects_cross_engine_extensions() {
        for pattern in [r"(?=secret)", r"(a)\1", r"\p{Greek}", r"\pL"] {
            let error = validate_portable_exclude_pattern(pattern).expect_err(pattern);
            assert_eq!(error.code(), INVALID_EXCLUDE_FILES_PATTERN_CODE);
            assert_eq!(error.message(), INVALID_EXCLUDE_FILES_PATTERN_MESSAGE);
        }
    }

    #[test]
    fn filter_only_changes_discovery() {
        let backend = Arc::new(MemoryWorkspaceBackend::default());
        backend
            .write_text("generated/cache.bin", "cache", false)
            .expect("write excluded file");
        backend
            .write_text("notes/readme.md", "notes", false)
            .expect("write visible file");
        let filtered =
            DiscoveryFilteredWorkspaceBackend::new(backend.clone(), r"^(?:generated|logs)/")
                .expect("filtered backend");

        assert_eq!(
            filtered.list_files(".", "**/*").expect("list files"),
            vec!["notes/readme.md"]
        );
        assert_eq!(
            filtered
                .read_text("generated/cache.bin")
                .expect("known path remains accessible"),
            "cache"
        );
        assert!(Arc::ptr_eq(
            filtered.inner(),
            &(backend as Arc<dyn WorkspaceBackend>)
        ));
    }
}
