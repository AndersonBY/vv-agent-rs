use std::sync::Arc;

use serde_json::{json, Value};

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{stringify_tool_arg, tool_error_with_code, tool_result_with_metadata};
use crate::types::{Metadata, ToolArguments, ToolDirective, ToolExecutionResult, ToolResultStatus};

pub fn compress_memory(
    context: &mut ToolContext,
    arguments: &ToolArguments,
) -> ToolExecutionResult {
    let spec = compress_memory_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn compress_memory_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "compress_memory",
        "Store key summary notes to reduce future context load.",
        Arc::new(|context, arguments| {
            let core_information = stringify_tool_arg(arguments.get("core_information"), "")
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
            tool_result_with_metadata(
                ToolResultStatus::Success,
                payload,
                None,
                ToolDirective::Continue,
                Metadata::from([("saved_notes".to_string(), json!(saved_notes))]),
            )
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("compress_memory") {
        spec.schema = schema;
    }
    spec
}
