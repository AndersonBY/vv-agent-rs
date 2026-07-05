use std::collections::BTreeMap;

use serde_json::Value;

mod command;
mod control;
mod media;
mod memory;
mod sub_agents;
mod todo;
mod workspace;

pub const WORKSPACE_TOOLS: &[&str] = &[
    "find_files",
    "file_info",
    "read_file",
    "write_file",
    "edit_file",
    "search_files",
    "compress_memory",
    "todo_write",
];

pub fn default_tool_schemas() -> BTreeMap<String, Value> {
    let mut schemas = BTreeMap::new();
    for (name, schema) in [
        ("task_finish", control::task_finish_schema()),
        ("ask_user", control::ask_user_schema()),
        ("activate_skill", control::activate_skill_schema()),
        ("read_file", workspace::read_file_schema()),
        ("write_file", workspace::write_file_schema()),
        ("find_files", workspace::find_files_schema()),
        ("file_info", workspace::file_info_schema()),
        ("search_files", workspace::search_files_schema()),
        ("edit_file", workspace::edit_file_schema()),
        ("compress_memory", memory::compress_memory_schema()),
        ("todo_write", todo::todo_write_schema()),
        ("bash", command::bash_schema()),
        (
            "check_background_command",
            command::check_background_command_schema(),
        ),
        ("create_sub_task", sub_agents::create_sub_task_schema()),
        ("sub_task_status", sub_agents::sub_task_status_schema()),
        ("read_image", media::read_image_schema()),
    ] {
        schemas.insert(name.to_string(), schema);
    }
    schemas
}

pub fn schema_for(name: &str) -> Option<Value> {
    default_tool_schemas().remove(name)
}
