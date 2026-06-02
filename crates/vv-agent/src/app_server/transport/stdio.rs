use crate::app_server::protocol::{AppServerError, JsonRpcMessage};

pub fn parse_jsonl_message(line: &str) -> Result<Option<JsonRpcMessage>, AppServerError> {
    if line.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str::<JsonRpcMessage>(line)
        .map(Some)
        .map_err(|error| AppServerError::invalid_request(error.to_string()))
}

pub fn serialize_jsonl_message(message: &JsonRpcMessage) -> Result<String, AppServerError> {
    let mut line = serde_json::to_string(message)
        .map_err(|error| AppServerError::internal(error.to_string()))?;
    line.push('\n');
    Ok(line)
}
