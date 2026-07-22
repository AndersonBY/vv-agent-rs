use std::collections::BTreeMap;

use serde_json::Value;

use crate::tools::base::ToolContext;
use crate::tools::common::{bool_arg, trim_portable_whitespace};
use crate::types::{Metadata, SubTaskRequest, ToolArguments, ToolExecutionResult};

pub(super) struct SubTaskArgumentError {
    message: String,
    error_code: &'static str,
}

impl SubTaskArgumentError {
    fn new(message: impl Into<String>, error_code: &'static str) -> Self {
        Self {
            message: message.into(),
            error_code,
        }
    }

    pub(super) fn into_tool_result(self) -> ToolExecutionResult {
        super::response::error_message(self.message, self.error_code)
    }
}

pub(super) struct SharedSubTaskOptions {
    pub(super) include_main_summary: bool,
    pub(super) exclude_files_pattern: Option<String>,
    pub(super) wait_for_completion: bool,
}

pub(super) struct BatchRequestEntry {
    pub(super) index: usize,
    pub(super) request: SubTaskRequest,
}

pub(super) enum SubTaskPayload {
    Single(SubTaskRequest),
    Batch {
        entries: Vec<BatchRequestEntry>,
        total: usize,
    },
}

impl SubTaskPayload {
    pub(super) fn extend_metadata(&mut self, metadata: &Metadata) {
        match self {
            Self::Single(request) => request.metadata.extend(metadata.clone()),
            Self::Batch { entries, .. } => {
                for entry in entries.iter_mut() {
                    entry.request.metadata.extend(metadata.clone());
                }
            }
        }
    }
}

pub(super) fn parent_lineage_metadata(context: &ToolContext) -> Metadata {
    let mut metadata = Metadata::new();
    let parent_run_id = context
        .run_context
        .as_ref()
        .map(|run| run.run_id.as_str())
        .filter(|value| !trim_portable_whitespace(value).is_empty())
        .or_else(|| {
            context
                .metadata
                .get("_vv_agent_run_id")
                .and_then(Value::as_str)
                .filter(|value| !trim_portable_whitespace(value).is_empty())
        });
    if let Some(parent_run_id) = parent_run_id {
        metadata.insert(
            "parent_run_id".to_string(),
            Value::String(parent_run_id.to_string()),
        );
    }
    if !trim_portable_whitespace(&context.tool_call_id).is_empty() {
        metadata.insert(
            "parent_tool_call_id".to_string(),
            Value::String(context.tool_call_id.clone()),
        );
    }
    metadata
}

pub(super) fn resolve_agent_name(
    arguments: &ToolArguments,
) -> Result<String, SubTaskArgumentError> {
    let agent_name = match arguments.get("agent_id") {
        Some(Value::String(value)) => trim_portable_whitespace(value).to_string(),
        Some(_) => {
            return Err(SubTaskArgumentError::new(
                "`agent_id` must be a string",
                "invalid_agent_id",
            ));
        }
        None => String::new(),
    };

    if agent_name.is_empty() {
        Err(SubTaskArgumentError::new(
            "`agent_id` is required",
            "agent_id_required",
        ))
    } else {
        Ok(agent_name)
    }
}

pub(super) fn shared_sub_task_options(
    arguments: &ToolArguments,
) -> Result<SharedSubTaskOptions, SubTaskArgumentError> {
    let exclude_files_pattern = match arguments.get("exclude_files_pattern") {
        None => None,
        Some(Value::String(value)) => {
            let pattern = trim_portable_whitespace(value);
            (!pattern.is_empty()).then(|| pattern.to_string())
        }
        Some(_) => {
            return Err(SubTaskArgumentError::new(
                "`exclude_files_pattern` must be a string",
                "invalid_exclude_files_pattern",
            ));
        }
    };
    Ok(SharedSubTaskOptions {
        include_main_summary: arguments
            .get("include_main_summary")
            .is_some_and(|value| bool_arg(Some(value), false)),
        exclude_files_pattern,
        wait_for_completion: arguments
            .get("wait_for_completion")
            .is_none_or(|value| bool_arg(Some(value), true)),
    })
}

pub(super) fn parse_sub_task_payload(
    arguments: &ToolArguments,
    agent_name: &str,
    options: &SharedSubTaskOptions,
) -> Result<SubTaskPayload, SubTaskArgumentError> {
    let task_description =
        optional_trimmed_string(arguments, "task_description", "invalid_tasks_payload")?
            .unwrap_or_default();
    let raw_tasks_value = arguments.get("tasks");
    let raw_tasks = raw_tasks_value.and_then(Value::as_array);

    if !task_description.is_empty() && raw_tasks_value.is_some() {
        return Err(SubTaskArgumentError::new(
            "`task_description` and `tasks` are mutually exclusive",
            "sub_task_payload_conflict",
        ));
    }
    if task_description.is_empty() && raw_tasks_value.is_none() {
        return Err(SubTaskArgumentError::new(
            "Provide either `task_description` or `tasks`",
            "sub_task_payload_missing",
        ));
    }
    if task_description.is_empty() && raw_tasks.is_none() {
        return Err(SubTaskArgumentError::new(
            "`tasks` must be a non-empty array",
            "invalid_tasks_payload",
        ));
    }

    if !task_description.is_empty() {
        return Ok(SubTaskPayload::Single(SubTaskRequest {
            agent_name: agent_name.to_string(),
            task_description,
            output_requirements: optional_trimmed_string(
                arguments,
                "output_requirements",
                "invalid_tasks_payload",
            )?
            .unwrap_or_default(),
            include_main_summary: options.include_main_summary,
            exclude_files_pattern: options.exclude_files_pattern.clone(),
            metadata: BTreeMap::new(),
        }));
    }

    let tasks = raw_tasks.expect("tasks checked");
    if tasks.is_empty() {
        return Err(SubTaskArgumentError::new(
            "`tasks` must be a non-empty array",
            "invalid_tasks_payload",
        ));
    }

    Ok(SubTaskPayload::Batch {
        entries: build_batch_requests(tasks, agent_name, options)?,
        total: tasks.len(),
    })
}

fn build_batch_requests(
    tasks: &[Value],
    agent_name: &str,
    options: &SharedSubTaskOptions,
) -> Result<Vec<BatchRequestEntry>, SubTaskArgumentError> {
    tasks
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let item = item.as_object().ok_or_else(|| {
                SubTaskArgumentError::new("Task item must be an object", "invalid_tasks_payload")
            })?;
            let task_description = match item.get("task_description") {
                Some(Value::String(value)) => trim_portable_whitespace(value).to_string(),
                Some(_) => {
                    return Err(SubTaskArgumentError::new(
                        "`task_description` must be a string",
                        "invalid_tasks_payload",
                    ));
                }
                None => String::new(),
            };
            if task_description.is_empty() {
                return Err(SubTaskArgumentError::new(
                    format!("`tasks[{index}].task_description` is required"),
                    "invalid_tasks_payload",
                ));
            }
            Ok(BatchRequestEntry {
                index,
                request: SubTaskRequest {
                    agent_name: agent_name.to_string(),
                    task_description,
                    output_requirements: match item.get("output_requirements") {
                        None => String::new(),
                        Some(Value::String(value)) => trim_portable_whitespace(value).to_string(),
                        Some(_) => {
                            return Err(SubTaskArgumentError::new(
                                "`output_requirements` must be a string",
                                "invalid_tasks_payload",
                            ));
                        }
                    },
                    include_main_summary: options.include_main_summary,
                    exclude_files_pattern: options.exclude_files_pattern.clone(),
                    metadata: BTreeMap::from([(
                        "batch_index".to_string(),
                        Value::Number((index as u64).into()),
                    )]),
                },
            })
        })
        .collect()
}

fn optional_trimmed_string(
    arguments: &ToolArguments,
    key: &'static str,
    error_code: &'static str,
) -> Result<Option<String>, SubTaskArgumentError> {
    match arguments.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(trim_portable_whitespace(value).to_string())),
        Some(_) => Err(SubTaskArgumentError::new(
            format!("`{key}` must be a string"),
            error_code,
        )),
    }
}
