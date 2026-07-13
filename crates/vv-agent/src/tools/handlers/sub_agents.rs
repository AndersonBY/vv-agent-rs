mod async_mode;
mod batch;
mod request;
mod response;

use std::sync::Arc;

use crate::tools::base::{ToolContext, ToolSpec};
use crate::types::{ToolArguments, ToolExecutionResult};
use crate::workspace::{
    validate_portable_exclude_pattern, INVALID_EXCLUDE_FILES_PATTERN_CODE,
    INVALID_EXCLUDE_FILES_PATTERN_MESSAGE,
};

use request::{
    parent_lineage_metadata, parse_sub_task_payload, resolve_agent_name, shared_sub_task_options,
    SubTaskPayload,
};

pub fn create_sub_task(
    context: &mut ToolContext,
    arguments: &ToolArguments,
) -> ToolExecutionResult {
    let spec = create_sub_task_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn create_sub_task_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "create_sub_task",
        "Create sub-tasks for a configured sub-agent.",
        Arc::new(handle_create_sub_task),
    );
    if let Some(schema) = super::super::schemas::schema_for("create_sub_task") {
        spec.schema = schema;
    }
    spec
}

fn handle_create_sub_task(
    context: &mut ToolContext,
    arguments: &ToolArguments,
) -> ToolExecutionResult {
    let Some(runner) = context.sub_task_runner.clone() else {
        return response::error_message(
            "Sub-agent runtime is not available for this task",
            "sub_agents_not_enabled",
        );
    };

    let agent_name = match resolve_agent_name(arguments) {
        Ok(agent_name) => agent_name,
        Err(error) => return error.into_tool_result(),
    };
    let options = match shared_sub_task_options(arguments) {
        Ok(options) => options,
        Err(error) => return error.into_tool_result(),
    };
    let mut payload = match parse_sub_task_payload(arguments, &agent_name, &options) {
        Ok(payload) => payload,
        Err(error) => return error.into_tool_result(),
    };
    if let SubTaskPayload::Batch { entries, total } = &payload {
        if !entries
            .iter()
            .any(|entry| entry.request.is_some() && entry.error.is_none())
        {
            return response::invalid_batch_payload(*total, entries, options.wait_for_completion);
        }
    }
    if let Some(pattern) = options.exclude_files_pattern.as_deref() {
        if validate_portable_exclude_pattern(pattern).is_err() {
            return response::error_message(
                INVALID_EXCLUDE_FILES_PATTERN_MESSAGE,
                INVALID_EXCLUDE_FILES_PATTERN_CODE,
            );
        }
    }
    payload.extend_metadata(&parent_lineage_metadata(context));

    match payload {
        SubTaskPayload::Single(request) if options.wait_for_completion => {
            response::format_single_sync_result(runner(request))
        }
        SubTaskPayload::Single(request) => async_mode::start_single_async(context, runner, request),
        SubTaskPayload::Batch { entries, total } => {
            if options.wait_for_completion {
                batch::run_batch_sync(context, runner, total, entries)
            } else {
                async_mode::start_batch_async(context, runner, &agent_name, total, entries)
            }
        }
    }
}
