mod info;
mod read;
mod write;

pub use info::file_info;
pub(crate) use info::file_info_tool;
pub use read::read_file;
pub(crate) use read::read_file_tool;
pub use write::write_file;
pub(crate) use write::write_file_tool;

pub(super) const READ_FILE_MAX_LINES: usize = 2_000;
pub(super) const READ_FILE_MAX_CHARS: usize = 50_000;
