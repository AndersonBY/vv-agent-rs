use std::collections::BTreeMap;

use jsonschema::{Draft, Validator};
use serde::Serialize;
use serde_json::{json, Value};

use crate::types::{Metadata, ToolCall, ToolDirective, ToolExecutionResult, ToolResultStatus};

pub(crate) const INVALID_TOOL_ARGUMENTS_ERROR_CODE: &str = "invalid_tool_arguments";
pub(crate) const INVALID_TOOL_ARGUMENTS_MESSAGE: &str =
    "Tool arguments do not match the declared schema";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct ArgumentValidationIssue {
    instance_path: String,
    schema_path: String,
    rule: String,
}

pub(crate) fn close_object_schemas(schema: &Value) -> Value {
    let mut closed = schema.clone();
    close_object_schemas_in_place(&mut closed);
    closed
}

pub(crate) fn validator_for_tool_schema(schema: &Value) -> Result<Validator, String> {
    let parameters = schema
        .get("function")
        .and_then(|function| function.get("parameters"))
        .ok_or_else(|| "tool schema must contain function.parameters".to_string())?;
    validator_for_parameters(parameters)
}

pub(crate) fn validator_for_parameters(schema: &Value) -> Result<Validator, String> {
    jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(schema)
        .map_err(|error| format!("invalid tool parameters schema: {error}"))
}

pub(crate) fn invalid_tool_arguments_result(
    validator: &Validator,
    call: &ToolCall,
) -> Option<ToolExecutionResult> {
    let instance = Value::Object(call.arguments.clone().into_iter().collect());
    let mut issues = validator
        .iter_errors(&instance)
        .map(|error| ArgumentValidationIssue {
            instance_path: error.instance_path().to_string(),
            schema_path: error.schema_path().to_string(),
            rule: error.kind().keyword().to_string(),
        })
        .collect::<Vec<_>>();
    issues.sort();
    issues.dedup();
    if issues.is_empty() {
        return None;
    }

    let mut metadata = Metadata::new();
    metadata.insert(
        "error_code".to_string(),
        json!(INVALID_TOOL_ARGUMENTS_ERROR_CODE),
    );
    metadata.insert("issue_count".to_string(), json!(issues.len()));
    Some(ToolExecutionResult {
        tool_call_id: call.id.clone(),
        content: json!({
            "ok": false,
            "error": INVALID_TOOL_ARGUMENTS_MESSAGE,
            "error_code": INVALID_TOOL_ARGUMENTS_ERROR_CODE,
            "issues": issues,
        })
        .to_string(),
        status: ToolResultStatus::Error,
        directive: ToolDirective::Continue,
        error_code: Some(INVALID_TOOL_ARGUMENTS_ERROR_CODE.to_string()),
        metadata,
        image_url: None,
        image_path: None,
    })
}

fn close_object_schemas_in_place(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("object") {
                object
                    .entry("additionalProperties".to_string())
                    .or_insert(Value::Bool(false));
            }
            for child in object.values_mut() {
                close_object_schemas_in_place(child);
            }
        }
        Value::Array(items) => {
            for child in items {
                close_object_schemas_in_place(child);
            }
        }
        _ => {}
    }
}

pub(crate) fn schema_map_with_closed_objects(
    schemas: BTreeMap<String, Value>,
) -> BTreeMap<String, Value> {
    schemas
        .into_iter()
        .map(|(name, schema)| (name, close_object_schemas(&schema)))
        .collect()
}
