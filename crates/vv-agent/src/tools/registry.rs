use std::collections::BTreeMap;

use serde_json::Value;

use super::base::{ToolContext, ToolHandler, ToolNotFoundError, ToolSpec};
use super::handlers::{
    background::check_background_command_tool,
    bash::bash_tool,
    control::{ask_user_tool, task_finish_tool, todo_write_tool},
    image::read_image_tool,
    memory::compress_memory_tool,
    search::workspace_grep_tool,
    skills::activate_skill_tool,
    sub_agents::create_sub_task_tool,
    sub_task_status::sub_task_status_tool,
    workspace_io::{
        file_info_tool, file_str_replace_tool, list_files_tool, read_file_tool, write_file_tool,
    },
};
use crate::types::{AgentTask, ToolCall, ToolExecutionResult};

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, ToolSpec>,
    schemas: BTreeMap<String, Value>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, mut spec: ToolSpec) -> Result<(), String> {
        if self.tools.contains_key(&spec.name) {
            return Err(format!("tool already registered: {}", spec.name));
        }
        if let Some(schema) = super::schemas::schema_for(&spec.name) {
            spec.schema = schema;
        }
        self.schemas.insert(spec.name.clone(), spec.schema.clone());
        self.tools.insert(spec.name.clone(), spec);
        Ok(())
    }

    pub fn register_many(&mut self, specs: Vec<ToolSpec>) -> Result<(), String> {
        for spec in specs {
            self.register(spec)?;
        }
        Ok(())
    }

    pub fn register_schema(&mut self, tool_name: impl Into<String>, schema: Value) {
        self.schemas.insert(tool_name.into(), schema);
    }

    pub fn register_schemas(&mut self, schemas: BTreeMap<String, Value>) {
        for (tool_name, schema) in schemas {
            self.register_schema(tool_name, schema);
        }
    }

    pub fn get(&self, name: &str) -> Result<&ToolSpec, ToolNotFoundError> {
        self.tools
            .get(name)
            .ok_or_else(|| ToolNotFoundError(name.to_string()))
    }

    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn has_schema(&self, name: &str) -> bool {
        self.schemas.contains_key(name)
    }

    pub fn get_schema(&self, name: &str) -> Option<Value> {
        self.schemas.get(name).cloned()
    }

    pub fn list_openai_schemas(&self, tool_names: Option<&[String]>) -> Vec<Value> {
        match tool_names {
            Some(names) => names
                .iter()
                .filter_map(|name| self.get_schema(name))
                .collect(),
            None => self.schemas.values().cloned().collect(),
        }
    }

    pub fn planned_tool_names(&self, task: &AgentTask) -> Vec<String> {
        let mut names = vec!["task_finish".to_string()];
        if task.allow_interruption {
            names.push("ask_user".to_string());
        }
        if task.use_workspace {
            names.extend(
                [
                    "list_files",
                    "file_info",
                    "read_file",
                    "write_file",
                    "file_str_replace",
                    "workspace_grep",
                    "compress_memory",
                ]
                .into_iter()
                .map(str::to_string),
            );
        }
        if task.agent_type.as_deref() == Some("computer") {
            names.push("bash".to_string());
            names.push("check_background_command".to_string());
        }
        if task.sub_agents_enabled() {
            names.push("create_sub_task".to_string());
            names.push("sub_task_status".to_string());
        }
        if task
            .metadata
            .get("available_skills")
            .is_some_and(|value| !value.is_null())
        {
            names.push("activate_skill".to_string());
        }
        if task.native_multimodal {
            names.push("read_image".to_string());
        }
        names.extend(task.extra_tool_names.clone());
        if !task.exclude_tools.is_empty() {
            names.retain(|name| !task.exclude_tools.contains(name));
        }
        let mut deduped = Vec::new();
        for name in names {
            if self.has_schema(&name) && !deduped.contains(&name) {
                deduped.push(name);
            }
        }
        deduped
    }

    pub fn planned_openai_schemas(&self, task: &AgentTask) -> Vec<Value> {
        let names = self.planned_tool_names(task);
        crate::runtime::patch_dynamic_tool_schema_hints(
            task,
            self.list_openai_schemas(Some(&names)),
        )
    }

    pub fn register_tool(
        &mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        handler: ToolHandler,
    ) -> Result<(), String> {
        self.register_tool_with_parameters(
            name,
            description,
            serde_json::json!({"type": "object", "properties": {}, "required": []}),
            handler,
        )
    }

    pub fn register_tool_with_parameters(
        &mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
        handler: ToolHandler,
    ) -> Result<(), String> {
        let name = name.into();
        let description = description.into();
        let mut spec = ToolSpec::new(name.clone(), description.clone(), handler);
        spec.schema = serde_json::json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": description,
                    "parameters": parameters,
                }
        });
        self.register(spec)
    }

    pub fn execute(
        &self,
        call: &ToolCall,
        context: &mut ToolContext,
    ) -> Result<ToolExecutionResult, ToolNotFoundError> {
        let tool = self.get(&call.name)?;
        Ok((tool.handler)(context, &call.arguments))
    }
}

pub fn build_default_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry
        .register(task_finish_tool())
        .expect("default task_finish registration");
    registry
        .register(ask_user_tool())
        .expect("default ask_user registration");
    registry
        .register(todo_write_tool())
        .expect("default todo_write registration");
    registry
        .register(compress_memory_tool())
        .expect("default compress_memory registration");
    registry
        .register(activate_skill_tool())
        .expect("default activate_skill registration");
    registry
        .register(list_files_tool())
        .expect("default list_files registration");
    registry
        .register(read_file_tool())
        .expect("default read_file registration");
    registry
        .register(write_file_tool())
        .expect("default write_file registration");
    registry
        .register(file_str_replace_tool())
        .expect("default file_str_replace registration");
    registry
        .register(workspace_grep_tool())
        .expect("default workspace_grep registration");
    registry
        .register(file_info_tool())
        .expect("default file_info registration");
    registry
        .register(read_image_tool())
        .expect("default read_image registration");
    registry
        .register(create_sub_task_tool())
        .expect("default create_sub_task registration");
    registry
        .register(sub_task_status_tool())
        .expect("default sub_task_status registration");
    registry
        .register(bash_tool())
        .expect("default bash registration");
    registry
        .register(check_background_command_tool())
        .expect("default check_background_command registration");
    registry
}
