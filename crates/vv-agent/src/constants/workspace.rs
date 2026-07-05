use std::collections::BTreeMap;

use serde_json::Value;

use super::tool_names::{
    ACTIVATE_SKILL_TOOL_NAME, ASK_USER_TOOL_NAME, COMPRESS_MEMORY_TOOL_NAME, EDIT_FILE_TOOL_NAME,
    FILE_INFO_TOOL_NAME, FIND_FILES_TOOL_NAME, READ_FILE_TOOL_NAME, SEARCH_FILES_TOOL_NAME,
    TASK_FINISH_TOOL_NAME, TODO_WRITE_TOOL_NAME, WRITE_FILE_TOOL_NAME,
};

pub const WORKSPACE_TOOLS: [&str; 8] = [
    FIND_FILES_TOOL_NAME,
    FILE_INFO_TOOL_NAME,
    READ_FILE_TOOL_NAME,
    WRITE_FILE_TOOL_NAME,
    EDIT_FILE_TOOL_NAME,
    SEARCH_FILES_TOOL_NAME,
    COMPRESS_MEMORY_TOOL_NAME,
    TODO_WRITE_TOOL_NAME,
];

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
