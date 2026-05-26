pub const TODO_INCOMPLETE_ERROR_CODE: &str = "todo_incomplete";

pub const ASK_USER_TOOL_NAME: &str = "ask_user";
pub const CREATE_SUB_TASK_TOOL_NAME: &str = "create_sub_task";
pub const SUB_TASK_STATUS_TOOL_NAME: &str = "sub_task_status";
pub const TASK_FINISH_TOOL_NAME: &str = "task_finish";
pub const READ_FILE_TOOL_NAME: &str = "read_file";
pub const WRITE_FILE_TOOL_NAME: &str = "write_file";
pub const LIST_FILES_TOOL_NAME: &str = "list_files";
pub const FILE_STR_REPLACE_TOOL_NAME: &str = "file_str_replace";
pub const WORKSPACE_GREP_TOOL_NAME: &str = "workspace_grep";
pub const BASH_TOOL_NAME: &str = "bash";
pub const CHECK_BACKGROUND_COMMAND_TOOL_NAME: &str = "check_background_command";
pub const COMPRESS_MEMORY_TOOL_NAME: &str = "compress_memory";
pub const TODO_WRITE_TOOL_NAME: &str = "todo_write";
pub const READ_IMAGE_TOOL_NAME: &str = "read_image";
pub const FILE_INFO_TOOL_NAME: &str = "file_info";
pub const ACTIVATE_SKILL_TOOL_NAME: &str = "activate_skill";

pub const WORKSPACE_TOOLS: [&str; 8] = [
    LIST_FILES_TOOL_NAME,
    FILE_INFO_TOOL_NAME,
    READ_FILE_TOOL_NAME,
    WRITE_FILE_TOOL_NAME,
    FILE_STR_REPLACE_TOOL_NAME,
    WORKSPACE_GREP_TOOL_NAME,
    COMPRESS_MEMORY_TOOL_NAME,
    TODO_WRITE_TOOL_NAME,
];

pub const DEFAULT_WORKSPACE_DIR: &str = "./workspace";
