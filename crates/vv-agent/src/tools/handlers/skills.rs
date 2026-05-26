use std::sync::Arc;

use serde_json::{json, Value};

use crate::tools::base::ToolSpec;
use crate::tools::common::{tool_error_with_code, tool_result};
use crate::types::{ToolDirective, ToolResultStatus};

pub(crate) fn activate_skill_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "activate_skill",
        "Activate a skill from the current task's available skill list.",
        Arc::new(|context, arguments| {
            let skill_name = arguments
                .get("skill_name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            if skill_name.is_empty() {
                return tool_error_with_code("`skill_name` is required", "skill_name_required");
            }
            let available = context
                .metadata
                .get("available_skills")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let found = available.iter().find(|skill| match skill {
                Value::String(name) => name == skill_name,
                Value::Object(object) => object
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name == skill_name),
                _ => false,
            });
            let Some(skill) = found else {
                return tool_error_with_code(
                    format!("skill not available: {skill_name}"),
                    "skill_not_available",
                );
            };
            let payload = json!({
                "ok": true,
                "skill_name": skill_name,
                "skill": skill,
                "reason": arguments.get("reason").cloned().unwrap_or(Value::Null),
            });
            tool_result(
                ToolResultStatus::Success,
                payload,
                None,
                ToolDirective::Continue,
            )
        }),
    );
    if let Some(schema) = super::super::schemas::schema_for("activate_skill") {
        spec.schema = schema;
    }
    spec
}
