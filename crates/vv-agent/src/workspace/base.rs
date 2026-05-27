use std::any::Any;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInfo {
    pub path: String,
    pub is_file: bool,
    pub is_dir: bool,
    pub size: u64,
    pub modified_at: String,
    pub suffix: String,
}

pub trait WorkspaceBackend: Any + Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn list_files(&self, base: &str, glob: &str) -> std::io::Result<Vec<String>>;
    fn read_text(&self, path: &str) -> std::io::Result<String>;
    fn read_bytes(&self, path: &str) -> std::io::Result<Vec<u8>>;
    fn write_text(&self, path: &str, content: &str, append: bool) -> std::io::Result<usize>;
    fn file_info(&self, path: &str) -> std::io::Result<Option<FileInfo>>;
    fn exists(&self, path: &str) -> bool;
    fn is_file(&self, path: &str) -> bool;
    fn mkdir(&self, path: &str) -> std::io::Result<()>;
}
