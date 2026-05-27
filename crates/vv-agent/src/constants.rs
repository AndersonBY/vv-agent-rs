use std::collections::BTreeMap;

use serde_json::Value;

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

pub fn get_default_tool_schemas() -> BTreeMap<String, Value> {
    crate::tools::schemas::default_tool_schemas()
}

pub fn workspace_tools_schemas() -> BTreeMap<String, Value> {
    let mut schemas = get_default_tool_schemas();
    schemas.retain(|name, _| WORKSPACE_TOOLS.contains(&name.as_str()));
    schemas
}

#[allow(non_snake_case)]
pub fn WORKSPACE_TOOLS_SCHEMAS() -> BTreeMap<String, Value> {
    workspace_tools_schemas()
}

pub fn task_finish_tool_schema() -> Value {
    schema_or_null(TASK_FINISH_TOOL_NAME)
}

#[allow(non_snake_case)]
pub fn TASK_FINISH_TOOL_SCHEMA() -> Value {
    task_finish_tool_schema()
}

pub fn ask_user_tool_schema() -> Value {
    schema_or_null(ASK_USER_TOOL_NAME)
}

#[allow(non_snake_case)]
pub fn ASK_USER_TOOL_SCHEMA() -> Value {
    ask_user_tool_schema()
}

pub fn activate_skill_tool_schema() -> Value {
    schema_or_null(ACTIVATE_SKILL_TOOL_NAME)
}

#[allow(non_snake_case)]
pub fn ACTIVATE_SKILL_TOOL_SCHEMA() -> Value {
    activate_skill_tool_schema()
}

fn schema_or_null(name: &str) -> Value {
    crate::tools::schemas::schema_for(name).unwrap_or(Value::Null)
}

pub mod tool_names {
    pub use super::{
        ACTIVATE_SKILL_TOOL_NAME, ASK_USER_TOOL_NAME, BASH_TOOL_NAME,
        CHECK_BACKGROUND_COMMAND_TOOL_NAME, COMPRESS_MEMORY_TOOL_NAME, CREATE_SUB_TASK_TOOL_NAME,
        FILE_INFO_TOOL_NAME, FILE_STR_REPLACE_TOOL_NAME, LIST_FILES_TOOL_NAME, READ_FILE_TOOL_NAME,
        READ_IMAGE_TOOL_NAME, SUB_TASK_STATUS_TOOL_NAME, TASK_FINISH_TOOL_NAME,
        TODO_INCOMPLETE_ERROR_CODE, TODO_WRITE_TOOL_NAME, WORKSPACE_GREP_TOOL_NAME,
        WRITE_FILE_TOOL_NAME,
    };
}

pub mod workspace {
    pub use super::{
        get_default_tool_schemas, workspace_tools_schemas, ACTIVATE_SKILL_TOOL_SCHEMA,
        ASK_USER_TOOL_SCHEMA, TASK_FINISH_TOOL_SCHEMA, WORKSPACE_TOOLS, WORKSPACE_TOOLS_SCHEMAS,
    };

    pub fn task_finish_tool_schema() -> serde_json::Value {
        super::task_finish_tool_schema()
    }

    pub fn ask_user_tool_schema() -> serde_json::Value {
        super::ask_user_tool_schema()
    }

    pub fn activate_skill_tool_schema() -> serde_json::Value {
        super::activate_skill_tool_schema()
    }
}
