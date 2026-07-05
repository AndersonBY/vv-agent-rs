use crate::tools::handlers::{
    background::check_background_command_tool,
    bash::bash_tool,
    control::{ask_user_tool, task_finish_tool},
    image::read_image_tool,
    memory::compress_memory_tool,
    search::search_files_tool,
    skills::activate_skill_tool,
    sub_agents::create_sub_task_tool,
    sub_task_status::sub_task_status_tool,
    todo::todo_write_tool,
    workspace::{
        edit::edit_file_tool, file_io::file_info_tool, file_io::read_file_tool,
        file_io::write_file_tool, listing::find_files_tool,
    },
};

use super::ToolRegistry;

pub fn build_default_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry
        .register(task_finish_tool())
        .expect("default task_finish registration");
    registry
        .register(ask_user_tool())
        .expect("default ask_user registration");
    registry
        .register(activate_skill_tool())
        .expect("default activate_skill registration");
    registry
        .register(todo_write_tool())
        .expect("default todo_write registration");
    registry
        .register(compress_memory_tool())
        .expect("default compress_memory registration");
    registry
        .register(find_files_tool())
        .expect("default find_files registration");
    registry
        .register(file_info_tool())
        .expect("default file_info registration");
    registry
        .register(read_file_tool())
        .expect("default read_file registration");
    registry
        .register(write_file_tool())
        .expect("default write_file registration");
    registry
        .register(edit_file_tool())
        .expect("default edit_file registration");
    registry
        .register(search_files_tool())
        .expect("default search_files registration");
    registry
        .register(bash_tool())
        .expect("default bash registration");
    registry
        .register(check_background_command_tool())
        .expect("default check_background_command registration");
    registry
        .register(create_sub_task_tool())
        .expect("default create_sub_task registration");
    registry
        .register(sub_task_status_tool())
        .expect("default sub_task_status registration");
    registry
        .register(read_image_tool())
        .expect("default read_image registration");
    registry
}
