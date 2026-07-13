mod state;

use std::sync::Arc;

use serde_json::Value;

use crate::skills::normalize_skill_list;
use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{tool_error_with_code, tool_result_with_metadata};
use crate::tools::schemas;
use crate::types::{Metadata, ToolArguments, ToolDirective, ToolExecutionResult, ToolResultStatus};

use state::{append_activation_log, append_unique_string};

pub fn activate_skill(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let spec = activate_skill_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn activate_skill_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "activate_skill",
        "Activate a skill from the current task's available skill list.",
        Arc::new(|context, arguments| {
            let skill_name = value_to_trimmed_string(arguments.get("skill_name"));
            let reason = value_to_trimmed_string(arguments.get("reason"));
            if skill_name.is_empty() {
                return tool_error_with_code("`skill_name` is required", "skill_name_required");
            }
            let raw_skills = context.shared_state.get("available_skills");
            let entries = normalize_skill_list(raw_skills, Some(&context.workspace), true);
            if entries.is_empty() {
                return tool_error_with_code(
                    "No skills are configured for this task",
                    "no_skills_configured",
                );
            }
            let Some(entry) = entries.into_iter().find(|entry| entry.name == skill_name) else {
                return tool_error_with_code(
                    format!("Skill '{skill_name}' is not allowed for this task"),
                    "skill_not_allowed",
                );
            };
            if let Some(error) = entry.load_error {
                return tool_error_with_code(
                    format!("Skill '{skill_name}' is invalid: {error}"),
                    "skill_invalid",
                );
            }

            let instructions = entry
                .instructions
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| {
                    format!("Skill '{skill_name}' is activated, but no instruction text is available. Please inspect the skill files or provide explicit instructions.")
                });
            append_unique_string(
                &mut context.shared_state,
                "active_skills",
                entry.name.clone(),
            );
            append_activation_log(
                &mut context.shared_state,
                entry.name.clone(),
                reason.clone(),
                context.cycle_index,
            );
            let mut payload = serde_json::Map::from_iter([
                ("status".to_string(), Value::String("activated".to_string())),
                ("skill_name".to_string(), Value::String(entry.name.clone())),
                (
                    "message".to_string(),
                    Value::String(format!(
                        "Skill '{}' has been activated. Follow the instructions below.",
                        entry.name
                    )),
                ),
                ("instructions".to_string(), Value::String(instructions)),
            ]);
            if !entry.description.is_empty() {
                payload.insert("description".to_string(), Value::String(entry.description));
            }
            if let Some(location) = entry.location {
                payload.insert("location".to_string(), Value::String(location));
            }
            if let Some(allowed_tools) = entry.allowed_tools {
                payload.insert("allowed_tools".to_string(), Value::String(allowed_tools));
            }
            if !reason.is_empty() {
                payload.insert("reason".to_string(), Value::String(reason));
            }
            tool_result_with_metadata(
                ToolResultStatus::Success,
                Value::Object(payload),
                None,
                ToolDirective::Continue,
                Metadata::from([("skill_name".to_string(), Value::String(entry.name))]),
            )
        }),
    );
    if let Some(schema) = schemas::schema_for("activate_skill") {
        spec.schema = schema;
    }
    spec
}

fn value_to_trimmed_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(Value::Null) | None => String::new(),
        Some(Value::Bool(true)) => "True".to_string(),
        Some(Value::Bool(false)) => "False".to_string(),
        Some(Value::Number(number)) => number.to_string().trim().to_string(),
        Some(other) => other.to_string().trim().to_string(),
    }
}
