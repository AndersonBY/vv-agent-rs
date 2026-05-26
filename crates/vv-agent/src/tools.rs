use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::types::{ToolArguments, ToolCall, ToolDirective, ToolExecutionResult};
use crate::workspace::WorkspaceBackend;
pub type ToolHandler =
    Arc<dyn Fn(&mut ToolContext, &ToolArguments) -> ToolExecutionResult + Send + Sync + 'static>;

#[derive(Clone)]
pub struct ToolContext {
    pub workspace: PathBuf,
    pub shared_state: BTreeMap<String, Value>,
    pub cycle_index: u32,
    pub task_id: String,
    pub metadata: BTreeMap<String, Value>,
    pub workspace_backend: Arc<dyn WorkspaceBackend>,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ToolContext")
            .field("workspace", &self.workspace)
            .field("shared_state", &self.shared_state)
            .field("cycle_index", &self.cycle_index)
            .field("task_id", &self.task_id)
            .field("metadata", &self.metadata)
            .finish_non_exhaustive()
    }
}

impl ToolContext {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        let workspace = workspace.into();
        Self {
            workspace: workspace.clone(),
            shared_state: BTreeMap::new(),
            cycle_index: 0,
            task_id: String::new(),
            metadata: BTreeMap::new(),
            workspace_backend: Arc::new(crate::workspace::LocalWorkspaceBackend::new(workspace)),
        }
    }
}

#[derive(Clone)]
pub struct ToolSpec {
    pub name: String,
    pub handler: ToolHandler,
    pub description: String,
    pub schema: Value,
}

impl ToolSpec {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        handler: ToolHandler,
    ) -> Self {
        let name = name.into();
        let description = description.into();
        Self {
            schema: json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": description,
                    "parameters": {"type": "object", "properties": {}, "required": []},
                }
            }),
            name,
            handler,
            description,
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("tool not found: {0}")]
pub struct ToolNotFoundError(pub String);

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, ToolSpec>,
    schemas: BTreeMap<String, Value>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, spec: ToolSpec) -> Result<(), String> {
        if self.tools.contains_key(&spec.name) {
            return Err(format!("tool already registered: {}", spec.name));
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

    pub fn register_tool(
        &mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        handler: ToolHandler,
    ) -> Result<(), String> {
        let spec = ToolSpec::new(name, description, handler);
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
        .register(file_info_tool())
        .expect("default file_info registration");
    registry
        .register(bash_tool())
        .expect("default bash registration");
    registry
}

pub fn dispatch_tool_call(
    registry: &ToolRegistry,
    call: &ToolCall,
    context: &mut ToolContext,
) -> Result<ToolExecutionResult, ToolNotFoundError> {
    registry.execute(call, context)
}

fn task_finish_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "task_finish",
        "Finish the current task and return the final answer to the user.",
        Arc::new(|context, arguments| {
            let message = arguments
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Task completed")
                .to_string();
            let require_all_done = arguments
                .get("require_all_todos_completed")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if require_all_done {
                let incomplete_todos = context
                    .shared_state
                    .get("todo_list")
                    .and_then(Value::as_array)
                    .map(|todos| {
                        todos
                            .iter()
                            .filter_map(|todo| {
                                let status = todo
                                    .get("status")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_ascii_lowercase();
                                let done =
                                    todo.get("done").and_then(Value::as_bool).unwrap_or(false);
                                if matches!(status.as_str(), "completed" | "done" | "finished")
                                    || done
                                {
                                    None
                                } else {
                                    Some(
                                        todo.get("title")
                                            .and_then(Value::as_str)
                                            .unwrap_or("Untitled TODO")
                                            .to_string(),
                                    )
                                }
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if !incomplete_todos.is_empty() {
                    return ToolExecutionResult {
                        tool_call_id: String::new(),
                        content: json!({
                            "ok": false,
                            "error_code": "todo_incomplete",
                            "error": "Cannot finish task while todo items are incomplete",
                            "incomplete_todos": incomplete_todos,
                        })
                        .to_string(),
                        status: crate::types::ToolResultStatus::Error,
                        directive: ToolDirective::Continue,
                        error_code: Some("todo_incomplete".to_string()),
                        metadata: BTreeMap::new(),
                        image_url: None,
                        image_path: None,
                    };
                }
            }
            let mut metadata = BTreeMap::new();
            metadata.insert("final_message".to_string(), Value::String(message.clone()));
            if let Some(exposed_files) = arguments.get("exposed_files").and_then(Value::as_array) {
                metadata.insert(
                    "exposed_files".to_string(),
                    Value::Array(exposed_files.clone()),
                );
            }
            ToolExecutionResult {
                tool_call_id: String::new(),
                content: json!({"ok": true, "message": message}).to_string(),
                status: crate::types::ToolResultStatus::Success,
                directive: ToolDirective::Finish,
                error_code: None,
                metadata,
                image_url: None,
                image_path: None,
            }
        }),
    );
    spec.schema = json!({
        "type": "function",
        "function": {
            "name": "task_finish",
            "description": "Finish the task with a final message.",
            "parameters": {
                "type": "object",
                "properties": {
                    "message": {"type": "string"},
                    "require_all_todos_completed": {"type": "boolean"},
                    "exposed_files": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["message"]
            }
        }
    });
    spec
}

fn ask_user_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "ask_user",
        "Ask the user a question and pause the agent until the user responds.",
        Arc::new(|_context, arguments| {
            let question = arguments
                .get("question")
                .and_then(Value::as_str)
                .unwrap_or("Need user input")
                .to_string();
            let selection_type = arguments
                .get("selection_type")
                .and_then(Value::as_str)
                .filter(|value| *value == "single" || *value == "multi")
                .unwrap_or("single")
                .to_string();
            let allow_custom_options = arguments
                .get("allow_custom_options")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let mut payload = BTreeMap::new();
            payload.insert("question".to_string(), Value::String(question.clone()));
            payload.insert("selection_type".to_string(), Value::String(selection_type));
            payload.insert(
                "allow_custom_options".to_string(),
                Value::Bool(allow_custom_options),
            );
            if let Some(options) = arguments.get("options").and_then(Value::as_array) {
                payload.insert("options".to_string(), Value::Array(options.clone()));
            }
            ToolExecutionResult {
                tool_call_id: String::new(),
                content: Value::Object(payload.clone().into_iter().collect()).to_string(),
                status: crate::types::ToolResultStatus::Success,
                directive: ToolDirective::WaitUser,
                error_code: None,
                metadata: payload,
                image_url: None,
                image_path: None,
            }
        }),
    );
    spec.schema = json!({
        "type": "function",
        "function": {
            "name": "ask_user",
            "description": "Ask the user for required input and pause execution.",
            "parameters": {
                "type": "object",
                "properties": {
                    "question": {"type": "string"},
                    "options": {"type": "array", "items": {"type": "string"}},
                    "selection_type": {"type": "string", "enum": ["single", "multi"]},
                    "allow_custom_options": {"type": "boolean"}
                },
                "required": ["question"]
            }
        }
    });
    spec
}

fn todo_write_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "todo_write",
        "Replace the current todo list for the task.",
        Arc::new(|context, arguments| {
            let todos = arguments
                .get("todos")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let in_progress_count = todos
                .iter()
                .filter(|todo| {
                    todo.get("status")
                        .and_then(Value::as_str)
                        .is_some_and(|status| status == "in_progress")
                })
                .count();
            if in_progress_count > 1 {
                return ToolExecutionResult {
                    tool_call_id: String::new(),
                    content: json!({
                        "ok": false,
                        "error_code": "multiple_in_progress_todos",
                        "error": "Only one todo item can be in progress at a time",
                    })
                    .to_string(),
                    status: crate::types::ToolResultStatus::Error,
                    directive: ToolDirective::Continue,
                    error_code: Some("multiple_in_progress_todos".to_string()),
                    metadata: BTreeMap::new(),
                    image_url: None,
                    image_path: None,
                };
            }
            context
                .shared_state
                .insert("todo_list".to_string(), Value::Array(todos.clone()));
            ToolExecutionResult::success(
                "",
                json!({
                    "ok": true,
                    "todo_count": todos.len(),
                    "todos": todos,
                })
                .to_string(),
            )
        }),
    );
    spec.schema = json!({
        "type": "function",
        "function": {
            "name": "todo_write",
            "description": "Replace the task todo list. At most one item may be in_progress.",
            "parameters": {
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": {"type": "string"},
                                "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]},
                                "priority": {"type": "string", "enum": ["low", "medium", "high"]}
                            },
                            "required": ["title", "status"]
                        }
                    }
                },
                "required": ["todos"]
            }
        }
    });
    spec
}

fn list_files_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "list_files",
        "List files in the current workspace.",
        Arc::new(|context, arguments| {
            let path = arguments.get("path").and_then(Value::as_str).unwrap_or(".");
            let glob = arguments
                .get("glob")
                .and_then(Value::as_str)
                .unwrap_or("**/*");
            let max_results = arguments
                .get("max_results")
                .and_then(Value::as_u64)
                .unwrap_or(500)
                .clamp(1, 5_000) as usize;
            let include_ignored = arguments
                .get("include_ignored")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            match context.workspace_backend.list_files(path, glob) {
                Ok(mut files) => {
                    let ignored_roots = if include_ignored || path != "." {
                        Vec::new()
                    } else {
                        collect_ignored_roots(&files)
                    };
                    if !include_ignored && path == "." {
                        files.retain(|path| {
                            path.split('/')
                                .next()
                                .is_none_or(|root| !is_ignored_root(root))
                        });
                    }
                    let count = files.len();
                    let returned = files.into_iter().take(max_results).collect::<Vec<_>>();
                    let mut payload = json!({
                        "files": returned,
                        "count": count,
                        "returned_count": count.min(max_results),
                        "truncated": count > max_results,
                        "max_results": max_results,
                    });
                    if count > max_results {
                        payload["remaining_count"] = Value::Number((count - max_results).into());
                    }
                    if !ignored_roots.is_empty() {
                        payload["ignored_roots"] = Value::Array(
                            ignored_roots
                                .into_iter()
                                .map(|path| json!({"path": path}))
                                .collect(),
                        );
                        payload["message"] = Value::String(
                            "Common dependency/cache directories are summarized by default."
                                .to_string(),
                        );
                    }
                    ToolExecutionResult::success("", payload.to_string())
                }
                Err(error) => tool_error(error.to_string()),
            }
        }),
    );
    spec.schema = json!({
        "type": "function",
        "function": {
            "name": "list_files",
            "description": "List files in the workspace.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "glob": {"type": "string"},
                    "max_results": {"type": "integer"}
                }
            }
        }
    });
    spec
}

fn file_info_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "file_info",
        "Return metadata for a workspace path.",
        Arc::new(|context, arguments| {
            let Some(path) = arguments.get("path").and_then(Value::as_str) else {
                return tool_error("missing required argument: path");
            };
            match context.workspace_backend.file_info(path) {
                Ok(Some(info)) => {
                    let mut payload = json!({
                        "path": info.path,
                        "exists": true,
                        "is_file": info.is_file,
                        "is_dir": info.is_dir,
                        "size": info.size,
                        "modified_at": info.modified_at,
                    });
                    if info.is_file {
                        payload["suffix"] = Value::String(info.suffix);
                    }
                    ToolExecutionResult::success("", payload.to_string())
                }
                Ok(None) => tool_error(format!("path not found: {path}")),
                Err(error) => tool_error(error.to_string()),
            }
        }),
    );
    spec.schema = json!({
        "type": "function",
        "function": {
            "name": "file_info",
            "description": "Return metadata for a workspace path.",
            "parameters": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }
        }
    });
    spec
}

fn read_file_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "read_file",
        "Read a text file from the current workspace.",
        Arc::new(|context, arguments| {
            let Some(path) = arguments.get("path").and_then(Value::as_str) else {
                return tool_error("missing required argument: path");
            };
            if !context.workspace_backend.is_file(path) {
                return tool_error(format!("file not found: {path}"));
            }
            let start_line = arguments
                .get("start_line")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .max(1) as usize;
            let end_line = arguments
                .get("end_line")
                .and_then(Value::as_u64)
                .map(|line| line.max(start_line as u64) as usize);
            let show_line_numbers = arguments
                .get("show_line_numbers")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            match context.workspace_backend.read_text(path) {
                Ok(text) => {
                    let lines = text.lines().collect::<Vec<_>>();
                    let start_index = start_line.saturating_sub(1).min(lines.len());
                    let end_index = end_line.unwrap_or(lines.len()).min(lines.len());
                    let selected = &lines[start_index..end_index];
                    let content = selected
                        .iter()
                        .enumerate()
                        .map(|(offset, line)| {
                            if show_line_numbers {
                                format!("{}: {line}", start_index + offset + 1)
                            } else {
                                (*line).to_string()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    ToolExecutionResult::success(
                        "",
                        json!({
                            "path": path,
                            "start_line": start_index + 1,
                            "end_line": start_index + selected.len(),
                            "show_line_numbers": show_line_numbers,
                            "content": content,
                        })
                        .to_string(),
                    )
                }
                Err(error) => tool_error(error.to_string()),
            }
        }),
    );
    spec.schema = json!({
        "type": "function",
        "function": {
            "name": "read_file",
            "description": "Read a text file from the workspace.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "start_line": {"type": "integer"},
                    "end_line": {"type": "integer"},
                    "show_line_numbers": {"type": "boolean"}
                },
                "required": ["path"]
            }
        }
    });
    spec
}

fn write_file_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "write_file",
        "Write a text file in the current workspace.",
        Arc::new(|context, arguments| {
            let Some(path) = arguments.get("path").and_then(Value::as_str) else {
                return tool_error("missing required argument: path");
            };
            let content = arguments
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let append = arguments
                .get("append")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let leading_newline = append
                && arguments
                    .get("leading_newline")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            let trailing_newline = append
                && arguments
                    .get("trailing_newline")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            let write_content = format!(
                "{}{}{}",
                if leading_newline { "\n" } else { "" },
                content,
                if trailing_newline { "\n" } else { "" }
            );
            match context
                .workspace_backend
                .write_text(path, &write_content, append)
            {
                Ok(written) => ToolExecutionResult::success(
                    "",
                    json!({
                        "ok": true,
                        "path": path,
                        "append": append,
                        "leading_newline": leading_newline,
                        "trailing_newline": trailing_newline,
                        "written_chars": written,
                    })
                    .to_string(),
                ),
                Err(error) => tool_error(error.to_string()),
            }
        }),
    );
    spec.schema = json!({
        "type": "function",
        "function": {
            "name": "write_file",
            "description": "Write a text file in the workspace.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"},
                    "append": {"type": "boolean"},
                    "leading_newline": {"type": "boolean"},
                    "trailing_newline": {"type": "boolean"}
                },
                "required": ["path", "content"]
            }
        }
    });
    spec
}

fn file_str_replace_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "file_str_replace",
        "Replace text in a workspace file.",
        Arc::new(|context, arguments| {
            let Some(path) = arguments.get("path").and_then(Value::as_str) else {
                return tool_error("missing required argument: path");
            };
            if !context.workspace_backend.is_file(path) {
                return tool_error(format!("file not found: {path}"));
            }
            let Some(old_str) = arguments.get("old_str").and_then(Value::as_str) else {
                return tool_error("missing required argument: old_str");
            };
            if old_str.is_empty() {
                return tool_error("`old_str` cannot be empty");
            }
            let new_str = arguments
                .get("new_str")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let replace_all = arguments
                .get("replace_all")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let max_replacements = arguments
                .get("max_replacements")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .max(1) as usize;
            match context.workspace_backend.read_text(path) {
                Ok(text) => {
                    let occurrence_count = text.matches(old_str).count();
                    if occurrence_count == 0 {
                        return tool_error("`old_str` not found in file");
                    }
                    let replaced_count = if replace_all {
                        occurrence_count
                    } else {
                        occurrence_count.min(max_replacements)
                    };
                    let replaced_text = if replace_all {
                        text.replace(old_str, new_str)
                    } else {
                        replace_n(&text, old_str, new_str, max_replacements)
                    };
                    match context
                        .workspace_backend
                        .write_text(path, &replaced_text, false)
                    {
                        Ok(_) => ToolExecutionResult::success(
                            "",
                            json!({
                                "ok": true,
                                "path": path,
                                "replaced_count": replaced_count,
                            })
                            .to_string(),
                        ),
                        Err(error) => tool_error(error.to_string()),
                    }
                }
                Err(error) => tool_error(error.to_string()),
            }
        }),
    );
    spec.schema = json!({
        "type": "function",
        "function": {
            "name": "file_str_replace",
            "description": "Replace text in a workspace file.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old_str": {"type": "string"},
                    "new_str": {"type": "string"},
                    "replace_all": {"type": "boolean"},
                    "max_replacements": {"type": "integer"}
                },
                "required": ["path", "old_str", "new_str"]
            }
        }
    });
    spec
}

fn bash_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "bash",
        "Run a shell command in the current workspace.",
        Arc::new(|context, arguments| {
            let command = arguments
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if command.is_empty() {
                return tool_error_with_code("`command` is required", "command_required");
            }
            let lowered = command.to_ascii_lowercase();
            for snippet in [
                "rm -rf /",
                "shutdown",
                "reboot",
                "mkfs",
                "dd if=/dev/zero of=/dev/",
            ] {
                if lowered.contains(snippet) {
                    return tool_error_with_code(
                        format!("dangerous command blocked: {snippet}"),
                        "dangerous_command",
                    );
                }
            }
            let exec_dir = arguments
                .get("exec_dir")
                .and_then(Value::as_str)
                .unwrap_or(".");
            let cwd = resolve_workspace_path(&context.workspace, exec_dir);
            if !cwd.is_dir() {
                return tool_error_with_code(
                    format!("exec_dir not found: {exec_dir}"),
                    "invalid_exec_dir",
                );
            }
            let output = if cfg!(target_os = "windows") {
                Command::new("cmd")
                    .args(["/C", &command])
                    .current_dir(&cwd)
                    .output()
            } else {
                Command::new("sh")
                    .args(["-lc", &command])
                    .current_dir(&cwd)
                    .output()
            };
            match output {
                Ok(output) => {
                    let mut combined = String::new();
                    combined.push_str(&String::from_utf8_lossy(&output.stdout));
                    combined.push_str(&String::from_utf8_lossy(&output.stderr));
                    let exit_code = output.status.code().unwrap_or(-1);
                    let cwd_payload = if cwd == context.workspace {
                        ".".to_string()
                    } else {
                        cwd.strip_prefix(&context.workspace)
                            .map(|path| path.to_string_lossy().replace('\\', "/"))
                            .unwrap_or_else(|_| cwd.to_string_lossy().to_string())
                    };
                    let payload = json!({
                        "cwd": cwd_payload,
                        "exit_code": exit_code,
                        "output": combined.chars().take(50_000).collect::<String>(),
                    });
                    if exit_code == 0 {
                        ToolExecutionResult::success("", payload.to_string())
                    } else {
                        ToolExecutionResult {
                            tool_call_id: String::new(),
                            content: payload.to_string(),
                            status: crate::types::ToolResultStatus::Error,
                            directive: ToolDirective::Continue,
                            error_code: Some("command_failed".to_string()),
                            metadata: BTreeMap::new(),
                            image_url: None,
                            image_path: None,
                        }
                    }
                }
                Err(error) => tool_error_with_code(error.to_string(), "command_failed"),
            }
        }),
    );
    spec.schema = json!({
        "type": "function",
        "function": {
            "name": "bash",
            "description": "Run a shell command in the workspace.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "exec_dir": {"type": "string"},
                    "timeout": {"type": "integer"},
                    "stdin": {"type": "string"},
                    "auto_confirm": {"type": "boolean"},
                    "run_in_background": {"type": "boolean"}
                },
                "required": ["command"]
            }
        }
    });
    spec
}

fn tool_error(message: impl Into<String>) -> ToolExecutionResult {
    tool_error_with_code(message, "")
}

fn tool_error_with_code(
    message: impl Into<String>,
    error_code: impl Into<String>,
) -> ToolExecutionResult {
    let error_code = error_code.into();
    ToolExecutionResult {
        tool_call_id: String::new(),
        content: json!({"ok": false, "error": message.into(), "error_code": error_code})
            .to_string(),
        status: crate::types::ToolResultStatus::Error,
        directive: ToolDirective::Continue,
        error_code: if error_code.is_empty() {
            None
        } else {
            Some(error_code)
        },
        metadata: BTreeMap::new(),
        image_url: None,
        image_path: None,
    }
}

fn resolve_workspace_path(workspace: &Path, path: &str) -> PathBuf {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        workspace.join(candidate)
    }
}

fn replace_n(text: &str, old_str: &str, new_str: &str, max_replacements: usize) -> String {
    let mut remaining = text;
    let mut replaced = String::new();
    let mut count = 0;
    while count < max_replacements {
        let Some(index) = remaining.find(old_str) else {
            break;
        };
        replaced.push_str(&remaining[..index]);
        replaced.push_str(new_str);
        remaining = &remaining[index + old_str.len()..];
        count += 1;
    }
    replaced.push_str(remaining);
    replaced
}

fn collect_ignored_roots(files: &[String]) -> Vec<String> {
    let mut roots = files
        .iter()
        .filter_map(|path| path.split('/').next())
        .filter(|root| is_ignored_root(root))
        .map(str::to_string)
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    roots
}

fn is_ignored_root(root: &str) -> bool {
    matches!(
        root.to_ascii_lowercase().as_str(),
        ".venv"
            | "venv"
            | "node_modules"
            | ".git"
            | "__pycache__"
            | ".pytest_cache"
            | ".mypy_cache"
            | ".ruff_cache"
            | ".idea"
            | ".vscode"
            | "dist"
            | "build"
            | ".next"
            | ".nuxt"
            | ".cache"
            | "target"
            | "vendor"
    )
}

#[allow(dead_code)]
fn workspace_relative_path(workspace: &Path, path: &Path) -> String {
    path.strip_prefix(workspace)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
