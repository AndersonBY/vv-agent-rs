use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};

use crate::background_sessions::background_session_manager;
use crate::processes::{
    read_captured_output, remove_captured_output, start_captured_process, wait_for_child,
};
use crate::types::{
    AgentStatus, SubTaskRequest, ToolArguments, ToolCall, ToolDirective, ToolExecutionResult,
};
use crate::workspace::WorkspaceBackend;
pub type ToolHandler =
    Arc<dyn Fn(&mut ToolContext, &ToolArguments) -> ToolExecutionResult + Send + Sync + 'static>;
pub type SubTaskRunner = Arc<
    dyn Fn(crate::types::SubTaskRequest) -> crate::types::SubTaskOutcome + Send + Sync + 'static,
>;

#[derive(Clone)]
pub struct ToolContext {
    pub workspace: PathBuf,
    pub shared_state: BTreeMap<String, Value>,
    pub cycle_index: u32,
    pub task_id: String,
    pub metadata: BTreeMap<String, Value>,
    pub workspace_backend: Arc<dyn WorkspaceBackend>,
    pub sub_task_runner: Option<SubTaskRunner>,
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
            .field("has_sub_task_runner", &self.sub_task_runner.is_some())
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
            sub_task_runner: None,
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
        .register(compress_memory_tool())
        .expect("default compress_memory registration");
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

fn compress_memory_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "compress_memory",
        "Store key summary notes to reduce future context load.",
        Arc::new(|context, arguments| {
            let core_information = arguments
                .get("core_information")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if core_information.is_empty() {
                return tool_error_with_code(
                    "`core_information` is required",
                    "core_information_required",
                );
            }

            let note = json!({
                "cycle_index": context.cycle_index,
                "core_information": core_information,
            });
            let notes = context
                .shared_state
                .entry("memory_notes".to_string())
                .or_insert_with(|| Value::Array(Vec::new()));
            if !notes.is_array() {
                *notes = Value::Array(Vec::new());
            }
            let saved_notes = {
                let notes = notes.as_array_mut().expect("memory_notes array");
                notes.push(note);
                notes.len()
            };
            let payload = json!({
                "ok": true,
                "saved_notes": saved_notes,
            });
            tool_result(
                crate::types::ToolResultStatus::Success,
                payload,
                None,
                ToolDirective::Continue,
            )
        }),
    );
    spec.schema = json!({
        "type": "function",
        "function": {
            "name": "compress_memory",
            "description": "Store key summary notes to reduce future context load.",
            "parameters": {
                "type": "object",
                "properties": {
                    "core_information": {"type": "string"}
                },
                "required": ["core_information"]
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

fn read_image_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "read_image",
        "Read image from workspace path or HTTP URL for multimodal follow-up.",
        Arc::new(|context, arguments| {
            let raw_path = arguments
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            if raw_path.is_empty() {
                return tool_error_with_code("`path` is required", "path_required");
            }
            if raw_path.starts_with("http://") || raw_path.starts_with("https://") {
                let payload = json!({
                    "status": "loaded",
                    "source": "url",
                    "image_url": raw_path,
                });
                let mut result = tool_result(
                    crate::types::ToolResultStatus::Success,
                    payload,
                    None,
                    ToolDirective::Continue,
                );
                result.image_url = Some(raw_path.to_string());
                return result;
            }
            if !context.workspace_backend.exists(raw_path)
                || !context.workspace_backend.is_file(raw_path)
            {
                return tool_error_with_code(
                    format!("image file not found: {raw_path}"),
                    "image_not_found",
                );
            }
            let suffix = Path::new(raw_path)
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| format!(".{}", ext.to_ascii_lowercase()))
                .unwrap_or_default();
            let mime_type = match suffix.as_str() {
                ".jpg" | ".jpeg" => "image/jpeg",
                ".png" => "image/png",
                ".webp" => "image/webp",
                ".bmp" => "image/bmp",
                _ => {
                    return tool_error_with_code(
                        format!("unsupported image format: {suffix}"),
                        "unsupported_image_format",
                    )
                }
            };
            let bytes = match context.workspace_backend.read_bytes(raw_path) {
                Ok(bytes) => bytes,
                Err(error) => return tool_error_with_code(error.to_string(), "image_not_found"),
            };
            const MAX_INLINE_IMAGE_BYTES: usize = 5 * 1024 * 1024;
            if bytes.len() > MAX_INLINE_IMAGE_BYTES {
                return tool_result(
                    crate::types::ToolResultStatus::Error,
                    json!({
                        "error": "image is too large for inline message transport",
                        "max_bytes": MAX_INLINE_IMAGE_BYTES,
                        "actual_bytes": bytes.len(),
                    }),
                    Some("image_too_large"),
                    ToolDirective::Continue,
                );
            }
            let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
            let image_url = format!("data:{mime_type};base64,{encoded}");
            let payload = json!({
                "status": "loaded",
                "source": "workspace",
                "image_path": raw_path,
                "mime_type": mime_type,
                "inline_transport": true,
            });
            let mut result = tool_result(
                crate::types::ToolResultStatus::Success,
                payload,
                None,
                ToolDirective::Continue,
            );
            result.image_url = Some(image_url);
            result.image_path = Some(raw_path.to_string());
            result
        }),
    );
    spec.schema = json!({
        "type": "function",
        "function": {
            "name": "read_image",
            "description": "Read image from workspace path or HTTP URL, then attach the image payload to the next LLM turn.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }
        }
    });
    spec
}

fn workspace_grep_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "workspace_grep",
        "Search workspace files with grep-style semantics.",
        Arc::new(|context, arguments| {
            let pattern = arguments
                .get("pattern")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if pattern.is_empty() {
                return tool_error("Search pattern is required");
            }
            let output_mode = arguments
                .get("output_mode")
                .and_then(Value::as_str)
                .unwrap_or("content");
            if !matches!(output_mode, "content" | "files_with_matches" | "count") {
                return tool_error(format!(
                    "Invalid `output_mode`: {output_mode}. Supported: content, count, files_with_matches"
                ));
            }
            let file_type = arguments
                .get("type")
                .and_then(Value::as_str)
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty());
            if let Some(file_type) = &file_type {
                if !is_supported_file_type(file_type) {
                    return tool_error(format!("Unsupported file type: {file_type}"));
                }
            }
            let path = arguments.get("path").and_then(Value::as_str).unwrap_or(".");
            let include_hidden = arguments
                .get("include_hidden")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let include_ignored = arguments
                .get("include_ignored")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let multiline = arguments
                .get("multiline")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let show_line_numbers = arguments.get("n").and_then(Value::as_bool).unwrap_or(true);
            let context_lines = arguments
                .get("c")
                .and_then(Value::as_u64)
                .map(|value| value as usize);
            let before_context = context_lines
                .or_else(|| {
                    arguments
                        .get("b")
                        .and_then(Value::as_u64)
                        .map(|v| v as usize)
                })
                .unwrap_or(0);
            let after_context = context_lines
                .or_else(|| {
                    arguments
                        .get("a")
                        .and_then(Value::as_u64)
                        .map(|v| v as usize)
                })
                .unwrap_or(0);
            let head_limit = arguments
                .get("head_limit")
                .or_else(|| arguments.get("max_results"))
                .and_then(Value::as_u64)
                .map(|value| value.max(1) as usize);
            let case_insensitive = if let Some(case_sensitive) =
                arguments.get("case_sensitive").and_then(Value::as_bool)
            {
                !case_sensitive
            } else if let Some(force_insensitive) = arguments.get("i").and_then(Value::as_bool) {
                force_insensitive
            } else {
                !pattern.chars().any(char::is_uppercase)
            };

            let target_path = resolve_workspace_path(&context.workspace, path);
            let mut candidate_files = Vec::new();
            if target_path.is_file() {
                candidate_files.push(target_path);
            } else {
                match collect_workspace_files(&target_path) {
                    Ok(files) => candidate_files = files,
                    Err(error) => return tool_error(error.to_string()),
                }
            }

            let mut searched_files = 0usize;
            let mut total_matches = 0usize;
            let mut files_with_matches = Vec::<String>::new();
            let mut file_counts = BTreeMap::<String, usize>::new();
            let mut rows = Vec::<Value>::new();

            for file_path in candidate_files {
                let relative_path =
                    workspace_relative_path_or_absolute(&context.workspace, &file_path);
                if !include_hidden && is_hidden_path(&relative_path) {
                    continue;
                }
                if !include_ignored
                    && path == "."
                    && relative_path.split('/').next().is_some_and(is_ignored_root)
                {
                    continue;
                }
                if !matches_file_type(&relative_path, file_type.as_deref()) {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&file_path) else {
                    continue;
                };
                searched_files += 1;
                let grep_options = GrepTextOptions {
                    case_insensitive,
                    multiline,
                    before_context,
                    after_context,
                    show_line_numbers,
                };
                let file_match_rows = grep_text(&relative_path, &text, &pattern, grep_options);
                let match_count = file_match_rows
                    .iter()
                    .filter(|row| {
                        row.get("is_match")
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
                    })
                    .count();
                if match_count == 0 {
                    continue;
                }
                total_matches += match_count;
                files_with_matches.push(relative_path.clone());
                file_counts.insert(relative_path, match_count);
                rows.extend(file_match_rows);
            }

            files_with_matches.sort();
            let total_result_items = match output_mode {
                "files_with_matches" => files_with_matches.len(),
                "count" => file_counts.len(),
                _ => rows.len(),
            };
            let mut head_limited = false;
            if let Some(limit) = head_limit {
                match output_mode {
                    "files_with_matches" => {
                        head_limited = files_with_matches.len() > limit;
                        files_with_matches.truncate(limit);
                    }
                    "count" => {
                        head_limited = file_counts.len() > limit;
                        if head_limited {
                            file_counts = file_counts.into_iter().take(limit).collect();
                        }
                    }
                    _ => {
                        head_limited = rows.len() > limit;
                        rows.truncate(limit);
                    }
                }
            }

            let summary = json!({
                "files_searched": searched_files,
                "files_with_matches": file_counts.len(),
                "total_matches": total_matches,
            });
            let mut payload = json!({
                "summary": summary,
                "pattern": pattern,
                "output_mode": output_mode,
                "head_limit": head_limit,
                "head_limited": head_limited,
                "total_result_items": total_result_items,
                "returned_count": match output_mode {
                    "files_with_matches" => files_with_matches.len(),
                    "count" => file_counts.len(),
                    _ => rows.len(),
                },
                "truncated": head_limited,
            });
            match output_mode {
                "files_with_matches" => payload["files"] = json!(files_with_matches),
                "count" => payload["file_counts"] = json!(file_counts),
                _ => payload["matches"] = Value::Array(rows),
            }
            tool_result(
                crate::types::ToolResultStatus::Success,
                payload,
                None,
                ToolDirective::Continue,
            )
        }),
    );
    spec.schema = json!({
        "type": "function",
        "function": {
            "name": "workspace_grep",
            "description": "Search workspace files with regex-like grep semantics.",
            "parameters": {
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"},
                    "path": {"type": "string"},
                    "glob": {"type": "string"},
                    "include_hidden": {"type": "boolean"},
                    "include_ignored": {"type": "boolean"},
                    "output_mode": {"type": "string", "enum": ["content", "files_with_matches", "count"]},
                    "b": {"type": "integer"},
                    "a": {"type": "integer"},
                    "c": {"type": "integer"},
                    "n": {"type": "boolean"},
                    "i": {"type": "boolean"},
                    "case_sensitive": {"type": "boolean"},
                    "type": {"type": "string"},
                    "multiline": {"type": "boolean"},
                    "head_limit": {"type": "integer"},
                    "max_results": {"type": "integer"}
                },
                "required": ["pattern"]
            }
        }
    });
    spec
}

fn create_sub_task_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "create_sub_task",
        "Create sub-tasks for a configured sub-agent.",
        Arc::new(|context, arguments| {
            let Some(runner) = context.sub_task_runner.clone() else {
                return tool_error_with_code(
                    "Sub-agent runtime is not available for this task",
                    "sub_agents_not_enabled",
                );
            };

            let agent_name = arguments
                .get("agent_id")
                .or_else(|| arguments.get("agent_name"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if agent_name.is_empty() {
                return tool_error_with_code("`agent_id` is required", "agent_id_required");
            }

            let task_description = arguments
                .get("task_description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            let raw_tasks = arguments.get("tasks").and_then(Value::as_array);
            if !task_description.is_empty() && raw_tasks.is_some() {
                return tool_error_with_code(
                    "`task_description` and `tasks` are mutually exclusive",
                    "sub_task_payload_conflict",
                );
            }
            if task_description.is_empty() && raw_tasks.is_none() {
                return tool_error_with_code(
                    "Provide either `task_description` or `tasks`",
                    "sub_task_payload_missing",
                );
            }

            let include_main_summary = arguments
                .get("include_main_summary")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let exclude_files_pattern = arguments
                .get("exclude_files_pattern")
                .and_then(Value::as_str)
                .map(str::to_string);

            if !task_description.is_empty() {
                let request = SubTaskRequest {
                    agent_name,
                    task_description,
                    output_requirements: arguments
                        .get("output_requirements")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                    include_main_summary,
                    exclude_files_pattern,
                    metadata: BTreeMap::new(),
                };
                let outcome = runner(request);
                let payload = outcome.to_value();
                if outcome.status == AgentStatus::Completed {
                    return tool_result(
                        crate::types::ToolResultStatus::Success,
                        payload,
                        None,
                        ToolDirective::Continue,
                    );
                }
                let error_code = if outcome.status == AgentStatus::WaitUser {
                    "sub_task_wait_user"
                } else {
                    "sub_task_failed"
                };
                return tool_result(
                    crate::types::ToolResultStatus::Error,
                    payload,
                    Some(error_code),
                    ToolDirective::Continue,
                );
            }

            let tasks = raw_tasks.expect("tasks checked");
            if tasks.is_empty() {
                return tool_error_with_code(
                    "`tasks` must be a non-empty array",
                    "invalid_tasks_payload",
                );
            }
            let mut results = Vec::new();
            let mut completed = 0usize;
            let mut failed = 0usize;
            for (index, item) in tasks.iter().enumerate() {
                let Some(task_description) = item
                    .get("task_description")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                else {
                    failed += 1;
                    results.push(json!({
                        "index": index,
                        "status": "failed",
                        "error": "`task_description` is required",
                    }));
                    continue;
                };
                let request = SubTaskRequest {
                    agent_name: agent_name.clone(),
                    task_description: task_description.to_string(),
                    output_requirements: item
                        .get("output_requirements")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                    include_main_summary,
                    exclude_files_pattern: exclude_files_pattern.clone(),
                    metadata: BTreeMap::from([(
                        "batch_index".to_string(),
                        Value::Number((index as u64).into()),
                    )]),
                };
                let outcome = runner(request);
                if outcome.status == AgentStatus::Completed {
                    completed += 1;
                } else {
                    failed += 1;
                }
                let mut payload = outcome.to_value();
                payload["index"] = Value::Number((index as u64).into());
                results.push(payload);
            }

            let payload = json!({
                "summary": {
                    "total": tasks.len(),
                    "completed": completed,
                    "failed": failed,
                },
                "results": results,
                "wait_for_completion": true,
            });
            if completed == 0 {
                return tool_result(
                    crate::types::ToolResultStatus::Error,
                    payload,
                    Some("create_sub_task_batch_failed"),
                    ToolDirective::Continue,
                );
            }
            tool_result(
                crate::types::ToolResultStatus::Success,
                payload,
                None,
                ToolDirective::Continue,
            )
        }),
    );
    spec.schema = json!({
        "type": "function",
        "function": {
            "name": "create_sub_task",
            "description": "Create sub-tasks for a configured sub-agent.",
            "parameters": {
                "type": "object",
                "properties": {
                    "agent_id": {"type": "string"},
                    "task_description": {"type": "string"},
                    "output_requirements": {"type": "string"},
                    "tasks": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "task_description": {"type": "string"},
                                "output_requirements": {"type": "string"}
                            },
                            "required": ["task_description"]
                        }
                    },
                    "include_main_summary": {"type": "boolean"},
                    "exclude_files_pattern": {"type": "string"},
                    "wait_for_completion": {"type": "boolean"}
                },
                "required": ["agent_id"]
            }
        }
    });
    spec
}

fn sub_task_status_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "sub_task_status",
        "Inspect status for background sub-tasks.",
        Arc::new(|_context, _arguments| {
            tool_error_with_code(
                "Sub-task manager is not available for this task",
                "sub_task_manager_unavailable",
            )
        }),
    );
    spec.schema = json!({
        "type": "function",
        "function": {
            "name": "sub_task_status",
            "description": "Inspect status for background sub-tasks.",
            "parameters": {
                "type": "object",
                "properties": {
                    "task_ids": {"type": "array", "items": {"type": "string"}},
                    "detail_level": {"type": "string", "enum": ["basic", "snapshot"]},
                    "message": {"type": "string"},
                    "workspace_file_limit": {"type": "integer"},
                    "wait_for_response": {"type": "boolean"}
                },
                "required": ["task_ids"]
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
            let timeout_seconds = arguments
                .get("timeout")
                .and_then(Value::as_u64)
                .unwrap_or(300)
                .clamp(1, 600);
            let stdin_text = arguments.get("stdin").and_then(Value::as_str);
            let auto_confirm = arguments
                .get("auto_confirm")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let run_in_background = arguments
                .get("run_in_background")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let prepared = prepare_shell_execution(&command, auto_confirm);
            let started = start_captured_process(&prepared.command, &cwd, stdin_text);
            let mut started = match started {
                Ok(started) => started,
                Err(error) => return tool_error_with_code(error.to_string(), "command_failed"),
            };

            if run_in_background {
                let session_id = background_session_manager().adopt_running_process(
                    command,
                    cwd,
                    timeout_seconds,
                    started.child,
                    started.output_path,
                    prepared.shell,
                );
                let payload = json!({
                    "status": "running",
                    "session_id": session_id,
                });
                return tool_result(
                    crate::types::ToolResultStatus::Running,
                    payload,
                    None,
                    ToolDirective::Continue,
                );
            }

            match wait_for_child(&mut started.child, Duration::from_secs(timeout_seconds)) {
                Ok(Some(exit_status)) => {
                    let output = read_captured_output(&started.output_path, 50_000);
                    remove_captured_output(&started.output_path);
                    let exit_code = exit_status.code().unwrap_or(-1);
                    let mut payload = json!({
                        "cwd": workspace_relative_path_or_absolute(&context.workspace, &cwd),
                        "exit_code": exit_code,
                        "output": output,
                    });
                    if let Some(shell) = prepared.shell {
                        payload["shell"] = Value::String(shell);
                    }
                    if exit_code == 0 {
                        ToolExecutionResult::success("", payload.to_string())
                    } else {
                        tool_result(
                            crate::types::ToolResultStatus::Error,
                            payload,
                            Some("command_failed"),
                            ToolDirective::Continue,
                        )
                    }
                }
                Ok(None) => {
                    let output = read_captured_output(&started.output_path, 50_000);
                    let session_id = background_session_manager().adopt_running_process(
                        command,
                        cwd,
                        timeout_seconds,
                        started.child,
                        started.output_path,
                        prepared.shell.clone(),
                    );
                    let mut payload = json!({
                        "status": "running",
                        "session_id": session_id,
                        "cwd": exec_dir,
                        "message": format!(
                            "command exceeded foreground timeout after {timeout_seconds} seconds and continues in background; use `check_background_command` with this session_id to inspect progress"
                        ),
                        "output": output,
                        "transitioned_to_background": true,
                    });
                    if let Some(shell) = prepared.shell {
                        payload["shell"] = Value::String(shell);
                    }
                    tool_result(
                        crate::types::ToolResultStatus::Running,
                        payload,
                        None,
                        ToolDirective::Continue,
                    )
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

fn check_background_command_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "check_background_command",
        "Check status and output for a background command.",
        Arc::new(|_context, arguments| {
            let session_id = arguments
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            if session_id.is_empty() {
                return tool_error_with_code("`session_id` is required", "session_id_required");
            }
            let payload = background_session_manager().check(session_id);
            match payload.get("status").and_then(Value::as_str) {
                Some("running") => tool_result(
                    crate::types::ToolResultStatus::Running,
                    payload,
                    None,
                    ToolDirective::Continue,
                ),
                Some("completed") => ToolExecutionResult::success("", payload.to_string()),
                _ => tool_result(
                    crate::types::ToolResultStatus::Error,
                    payload,
                    Some("background_command_failed"),
                    ToolDirective::Continue,
                ),
            }
        }),
    );
    spec.schema = json!({
        "type": "function",
        "function": {
            "name": "check_background_command",
            "description": "Check status/output for a background command.",
            "parameters": {
                "type": "object",
                "properties": {
                    "session_id": {"type": "string"}
                },
                "required": ["session_id"]
            }
        }
    });
    spec
}

struct PreparedShellCommand {
    command: Vec<String>,
    shell: Option<String>,
}

fn prepare_shell_execution(command: &str, auto_confirm: bool) -> PreparedShellCommand {
    if cfg!(target_os = "windows") {
        PreparedShellCommand {
            command: vec!["cmd".to_string(), "/C".to_string(), command.to_string()],
            shell: Some("cmd".to_string()),
        }
    } else {
        let prepared_command = if auto_confirm {
            format!("yes | ({command})")
        } else {
            command.to_string()
        };
        PreparedShellCommand {
            command: vec!["sh".to_string(), "-lc".to_string(), prepared_command],
            shell: Some("bash".to_string()),
        }
    }
}

fn tool_error(message: impl Into<String>) -> ToolExecutionResult {
    tool_error_with_code(message, "")
}

fn tool_result(
    status: crate::types::ToolResultStatus,
    content: Value,
    error_code: Option<&str>,
    directive: ToolDirective,
) -> ToolExecutionResult {
    let metadata = content
        .as_object()
        .map(|object| {
            object
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    ToolExecutionResult {
        tool_call_id: String::new(),
        content: content.to_string(),
        status,
        directive,
        error_code: error_code.map(str::to_string),
        metadata,
        image_url: None,
        image_path: None,
    }
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

fn collect_workspace_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut stack = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(path) = stack.pop() {
        if path.is_file() {
            files.push(path);
            continue;
        }
        if !path.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&path)? {
            let entry = entry?;
            let entry_path = entry.path();
            if entry_path.is_dir() {
                stack.push(entry_path);
            } else if entry_path.is_file() {
                files.push(entry_path);
            }
        }
    }
    files.sort();
    Ok(files)
}

#[derive(Clone, Copy)]
struct GrepTextOptions {
    case_insensitive: bool,
    multiline: bool,
    before_context: usize,
    after_context: usize,
    show_line_numbers: bool,
}

fn grep_text(
    relative_path: &str,
    text: &str,
    pattern: &str,
    options: GrepTextOptions,
) -> Vec<Value> {
    if options.multiline {
        let haystack = if options.case_insensitive {
            text.to_ascii_lowercase()
        } else {
            text.to_string()
        };
        let needle = if options.case_insensitive {
            pattern.to_ascii_lowercase()
        } else {
            pattern.to_string()
        };
        if !haystack.contains(&needle) {
            return Vec::new();
        }
        let line = text[..haystack.find(&needle).unwrap_or(0)]
            .chars()
            .filter(|ch| *ch == '\n')
            .count()
            + 1;
        return vec![json!({
            "path": relative_path,
            "line": line,
            "text": pattern,
            "is_match": true,
        })];
    }

    let lines = text.lines().collect::<Vec<_>>();
    let mut include_lines = BTreeMap::<usize, bool>::new();
    for (index, line) in lines.iter().enumerate() {
        let matched = line_contains(line, pattern, options.case_insensitive);
        if !matched {
            continue;
        }
        let start = index.saturating_sub(options.before_context);
        let end = (index + options.after_context).min(lines.len().saturating_sub(1));
        for row_index in start..=end {
            include_lines.entry(row_index).or_insert(false);
        }
        include_lines.insert(index, true);
    }

    include_lines
        .into_iter()
        .map(|(index, is_match)| {
            let line_number = index + 1;
            let mut row = json!({
                "path": relative_path,
                "line": line_number,
                "text": lines[index],
                "is_match": is_match,
            });
            if !options.show_line_numbers {
                row.as_object_mut().expect("row object").remove("line");
            }
            row
        })
        .collect()
}

fn line_contains(line: &str, pattern: &str, case_insensitive: bool) -> bool {
    if case_insensitive {
        line.to_ascii_lowercase()
            .contains(&pattern.to_ascii_lowercase())
    } else {
        line.contains(pattern)
    }
}

fn is_hidden_path(path: &str) -> bool {
    path.split('/').any(|part| part.starts_with('.'))
}

fn is_supported_file_type(file_type: &str) -> bool {
    matches!(
        file_type,
        "py" | "js"
            | "ts"
            | "html"
            | "css"
            | "java"
            | "c"
            | "cpp"
            | "rust"
            | "go"
            | "php"
            | "rb"
            | "sh"
            | "sql"
            | "json"
            | "xml"
            | "yaml"
            | "md"
            | "txt"
            | "log"
            | "ini"
            | "dockerfile"
            | "makefile"
    )
}

fn matches_file_type(path: &str, file_type: Option<&str>) -> bool {
    let Some(file_type) = file_type else {
        return !is_binary_path(path);
    };
    let lower = path.to_ascii_lowercase();
    let filename = Path::new(&lower)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let suffix = Path::new(&lower)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| format!(".{ext}"))
        .unwrap_or_default();
    match file_type {
        "py" => matches!(suffix.as_str(), ".py" | ".pyw" | ".pyi"),
        "js" => matches!(suffix.as_str(), ".js" | ".jsx" | ".mjs"),
        "ts" => matches!(suffix.as_str(), ".ts" | ".tsx"),
        "html" => matches!(suffix.as_str(), ".html" | ".htm" | ".xhtml"),
        "css" => matches!(suffix.as_str(), ".css" | ".scss" | ".sass" | ".less"),
        "java" => suffix == ".java",
        "c" => matches!(suffix.as_str(), ".c" | ".h"),
        "cpp" => matches!(
            suffix.as_str(),
            ".cpp" | ".cc" | ".cxx" | ".c++" | ".hpp" | ".hh" | ".hxx" | ".h++"
        ),
        "rust" => suffix == ".rs",
        "go" => suffix == ".go",
        "php" => matches!(suffix.as_str(), ".php" | ".php3" | ".php4" | ".php5"),
        "rb" => matches!(suffix.as_str(), ".rb" | ".rbx" | ".rhtml" | ".ruby"),
        "sh" => matches!(suffix.as_str(), ".sh" | ".bash" | ".zsh" | ".fish"),
        "sql" => suffix == ".sql",
        "json" => suffix == ".json",
        "xml" => matches!(suffix.as_str(), ".xml" | ".xsl" | ".xsd"),
        "yaml" => matches!(suffix.as_str(), ".yaml" | ".yml"),
        "md" => matches!(suffix.as_str(), ".md" | ".markdown" | ".mdown" | ".mkd"),
        "txt" => suffix == ".txt",
        "log" => suffix == ".log",
        "ini" => matches!(suffix.as_str(), ".ini" | ".cfg" | ".conf"),
        "dockerfile" => filename == "dockerfile",
        "makefile" => matches!(filename, "makefile" | "gnumakefile"),
        _ => false,
    }
}

fn is_binary_path(path: &str) -> bool {
    let suffix = Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| format!(".{}", ext.to_ascii_lowercase()))
        .unwrap_or_default();
    matches!(
        suffix.as_str(),
        ".png"
            | ".jpg"
            | ".jpeg"
            | ".gif"
            | ".webp"
            | ".bmp"
            | ".ico"
            | ".pdf"
            | ".zip"
            | ".tar"
            | ".gz"
            | ".bz2"
            | ".xz"
            | ".7z"
            | ".rar"
            | ".mp3"
            | ".wav"
            | ".mp4"
            | ".mov"
            | ".avi"
            | ".mkv"
            | ".exe"
            | ".dll"
            | ".so"
            | ".dylib"
            | ".bin"
    )
}

fn workspace_relative_path_or_absolute(workspace: &Path, path: &Path) -> String {
    if path == workspace {
        return ".".to_string();
    }
    path.strip_prefix(workspace)
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"))
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
