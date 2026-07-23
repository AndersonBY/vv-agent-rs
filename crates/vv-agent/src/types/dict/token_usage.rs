use super::*;

pub(super) fn task_token_usage_to_dict(usage: &TaskTokenUsage) -> Value {
    serde_json::to_value(usage).expect("TaskTokenUsage is serializable")
}

pub(super) fn task_token_usage_from_dict(value: &Value) -> Result<TaskTokenUsage, String> {
    serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid TaskTokenUsage: {error}"))
}
