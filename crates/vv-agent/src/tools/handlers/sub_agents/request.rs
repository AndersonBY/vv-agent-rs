use std::collections::BTreeMap;

use serde_json::Value;

use crate::tools::common::{coerce_bool, stringify_tool_arg, tool_error_with_code};
use crate::types::{SubTaskRequest, ToolArguments, ToolExecutionResult};

pub(super) struct SubTaskArgumentError {
    message: &'static str,
    error_code: &'static str,
}

impl SubTaskArgumentError {
    fn new(message: &'static str, error_code: &'static str) -> Self {
        Self {
            message,
            error_code,
        }
    }

    pub(super) fn into_tool_result(self) -> ToolExecutionResult {
        tool_error_with_code(self.message, self.error_code)
    }
}

pub(super) struct SharedSubTaskOptions {
    pub(super) include_main_summary: bool,
    pub(super) exclude_files_pattern: Option<String>,
    pub(super) wait_for_completion: bool,
}

pub(super) struct BatchRequestEntry {
    pub(super) index: usize,
    pub(super) request: Option<SubTaskRequest>,
    pub(super) error: Option<String>,
}

pub(super) enum SubTaskPayload {
    Single(SubTaskRequest),
    Batch {
        entries: Vec<BatchRequestEntry>,
        total: usize,
    },
}

pub(super) fn resolve_agent_name(
    arguments: &ToolArguments,
) -> Result<String, SubTaskArgumentError> {
    let agent_name = arguments
        .get("agent_id")
        .filter(|raw| !raw.is_null())
        .map(|raw| stringify_tool_arg(Some(raw), "").trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default();

    if agent_name.is_empty() {
        Err(SubTaskArgumentError::new(
            "`agent_id` is required",
            "agent_id_required",
        ))
    } else {
        Ok(agent_name)
    }
}

pub(super) fn shared_sub_task_options(arguments: &ToolArguments) -> SharedSubTaskOptions {
    SharedSubTaskOptions {
        include_main_summary: arguments
            .get("include_main_summary")
            .is_some_and(|value| coerce_bool(Some(value), false)),
        exclude_files_pattern: arguments
            .get("exclude_files_pattern")
            .filter(|value| !value.is_null())
            .map(|value| stringify_tool_arg(Some(value), "").trim().to_string()),
        wait_for_completion: arguments
            .get("wait_for_completion")
            .is_none_or(|value| coerce_bool(Some(value), true)),
    }
}

pub(super) fn parse_sub_task_payload(
    arguments: &ToolArguments,
    agent_name: &str,
    options: &SharedSubTaskOptions,
) -> Result<SubTaskPayload, SubTaskArgumentError> {
    let task_description = arguments
        .get("task_description")
        .map(|value| stringify_tool_arg(Some(value), ""))
        .unwrap_or_default()
        .trim()
        .to_string();
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
            output_requirements: arguments
                .get("output_requirements")
                .map(|value| stringify_tool_arg(Some(value), ""))
                .unwrap_or_default()
                .trim()
                .to_string(),
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
        entries: build_batch_requests(tasks, agent_name, options),
        total: tasks.len(),
    })
}

fn build_batch_requests(
    tasks: &[Value],
    agent_name: &str,
    options: &SharedSubTaskOptions,
) -> Vec<BatchRequestEntry> {
    tasks
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let Some(item) = item.as_object() else {
                return BatchRequestEntry {
                    index,
                    request: None,
                    error: Some("Task item must be an object".to_string()),
                };
            };
            let task_description = item
                .get("task_description")
                .map(|value| stringify_tool_arg(Some(value), ""))
                .unwrap_or_default()
                .trim()
                .to_string();
            if task_description.is_empty() {
                return BatchRequestEntry {
                    index,
                    request: None,
                    error: Some("`task_description` is required".to_string()),
                };
            }
            BatchRequestEntry {
                index,
                request: Some(SubTaskRequest {
                    agent_name: agent_name.to_string(),
                    task_description,
                    output_requirements: item
                        .get("output_requirements")
                        .map(|value| stringify_tool_arg(Some(value), ""))
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                    include_main_summary: options.include_main_summary,
                    exclude_files_pattern: options.exclude_files_pattern.clone(),
                    metadata: BTreeMap::from([(
                        "batch_index".to_string(),
                        Value::Number((index as u64).into()),
                    )]),
                }),
                error: None,
            }
        })
        .collect()
}
