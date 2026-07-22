use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{bool_arg, string_arg};
use crate::types::{ToolArguments, ToolDirective, ToolExecutionResult, ToolResultStatus};

pub fn ask_user(context: &mut ToolContext, arguments: &ToolArguments) -> ToolExecutionResult {
    let spec = ask_user_tool();
    (spec.handler)(context, arguments)
}

pub(crate) fn ask_user_tool() -> ToolSpec {
    let mut spec = ToolSpec::new(
        "ask_user",
        "Ask the user a question and pause the agent until the user responds.",
        Arc::new(|_context, arguments| {
            let question = arguments
                .get("question")
                .map(|value| string_arg(Some(value), "").trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "Need user input".to_string());
            let selection_type = arguments
                .get("selection_type")
                .map(|value| string_arg(Some(value), "").trim().to_string())
                .filter(|value| value == "single" || value == "multi")
                .unwrap_or_else(|| "single".to_string());
            let allow_custom_options = arguments
                .get("allow_custom_options")
                .map(|value| bool_arg(Some(value), false))
                .unwrap_or(false);
            let mut payload = BTreeMap::new();
            payload.insert("question".to_string(), Value::String(question.clone()));
            payload.insert("selection_type".to_string(), Value::String(selection_type));
            payload.insert(
                "allow_custom_options".to_string(),
                Value::Bool(allow_custom_options),
            );
            if let Some(options) = normalize_ask_user_options(arguments.get("options")) {
                payload.insert("options".to_string(), Value::Array(options));
            }
            ToolExecutionResult {
                tool_call_id: String::new(),
                content: Value::Object(payload.clone().into_iter().collect()).to_string(),
                status: ToolResultStatus::Success,
                directive: ToolDirective::WaitUser,
                error_code: None,
                metadata: payload,
                image_url: None,
                image_path: None,
            }
        }),
    );
    if let Some(schema) = crate::tools::schemas::schema_for("ask_user") {
        spec.schema = schema;
    }
    spec
}

fn normalize_ask_user_options(raw: Option<&Value>) -> Option<Vec<Value>> {
    let options = raw?.as_array()?;
    let mut normalized = Vec::new();
    for option in options {
        let option_text = string_arg(Some(option), "").trim().to_string();
        if option_text.is_empty() {
            continue;
        }
        if normalized
            .iter()
            .any(|seen: &Value| seen.as_str() == Some(option_text.as_str()))
        {
            continue;
        }
        normalized.push(Value::String(option_text));
    }
    (!normalized.is_empty()).then_some(normalized)
}
