use std::sync::Arc;

use serde_json::{json, Value};

use crate::runtime::background_sessions::background_session_manager;
use crate::tools::base::{ToolContext, ToolSpec};
use crate::tools::common::{string_arg, tool_error_with_code};
use crate::types::{
    Metadata, ToolArguments, ToolArtifactRef, ToolExecutionResult, ToolResultStatus,
    ToolTruncationReason,
};

pub fn check_background_command(
    context: &mut ToolContext,
    arguments: &ToolArguments,
) -> ToolExecutionResult {
    manage(context, arguments, false)
}

pub fn stop_background_command(
    context: &mut ToolContext,
    arguments: &ToolArguments,
) -> ToolExecutionResult {
    manage(context, arguments, true)
}

pub(crate) fn check_background_command_tool() -> ToolSpec {
    management_spec("check_background_command", false)
}

pub(crate) fn stop_background_command_tool() -> ToolSpec {
    management_spec("stop_background_command", true)
}

fn management_spec(name: &str, stop: bool) -> ToolSpec {
    let mut spec = ToolSpec::new(
        name,
        "Manage a command in the owning task and workspace.",
        Arc::new(move |context, arguments| manage(context, arguments, stop)),
    );
    if let Some(schema) = super::super::schemas::schema_for(name) {
        spec.schema = schema;
    }
    spec
}

fn manage(context: &mut ToolContext, arguments: &ToolArguments, stop: bool) -> ToolExecutionResult {
    let session_id = string_arg(arguments.get("session_id"), "");
    if session_id.trim().is_empty() {
        return tool_error_with_code("`session_id` is required", "session_id_required");
    }
    let manager = background_session_manager();
    let payload = if stop {
        manager.stop_for_tool(
            session_id.trim(),
            context.effective_workspace_backend(),
            &context.task_id,
            &context.tool_call_id,
            &context.workspace,
        )
    } else {
        manager.check_for_tool(
            session_id.trim(),
            context.effective_workspace_backend(),
            &context.task_id,
            &context.tool_call_id,
            &context.workspace,
        )
    };
    background_command_result(payload, None, false)
}

pub(super) fn background_command_result(
    mut payload: Value,
    foreground_cwd: Option<String>,
    force_handle: bool,
) -> ToolExecutionResult {
    let status = payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("missing")
        .to_string();
    let mut metadata = background_metadata(&payload);
    if let Some(error) = payload.get("artifact_error").and_then(Value::as_str) {
        return management_error(
            format!("failed to persist complete command output: {error}"),
            payload
                .get("artifact_error_code")
                .and_then(Value::as_str)
                .unwrap_or("artifact_persist_failed"),
            metadata,
        );
    }
    if let Some(error) = payload.get("output_error").and_then(Value::as_str) {
        return management_error(
            format!("failed to read command output: {error}"),
            "command_failed",
            metadata,
        );
    }
    if !matches!(
        status.as_str(),
        "running" | "stopping" | "unknown" | "completed" | "failed" | "timeout" | "stopped"
    ) {
        let error = payload
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("Background command failed")
            .to_string();
        let code = payload
            .get("error_code")
            .and_then(Value::as_str)
            .unwrap_or("background_command_failed")
            .to_string();
        payload["ok"] = json!(false);
        payload["error"] = json!(error);
        payload["error_code"] = json!(code);
        metadata.insert("error_code".to_string(), json!(code));
        let mut result = ToolExecutionResult::success("", payload.to_string());
        result.status = ToolResultStatus::Error;
        result.error_code = Some(code);
        result.metadata = metadata;
        return result;
    }
    let truncated = payload.get("output_truncated").and_then(Value::as_bool) == Some(true);
    let mut result = ToolExecutionResult::success("", "");
    if truncated {
        let artifact = payload
            .get("artifact")
            .cloned()
            .and_then(|value| serde_json::from_value::<ToolArtifactRef>(value).ok());
        let Some(artifact) = artifact else {
            return management_error(
                "complete command output has no recoverable artifact",
                "artifact_persist_failed",
                metadata,
            );
        };
        result.truncated = true;
        result.truncation_reason = Some(ToolTruncationReason::OutputLimit);
        result.artifact = Some(artifact);
    }
    let ongoing = matches!(status.as_str(), "running" | "stopping" | "unknown");
    let exit_code = payload.get("exit_code").and_then(Value::as_i64);
    let success = ongoing || (status != "timeout" && exit_code == Some(0));
    let code = if foreground_cwd.is_some() {
        "command_failed"
    } else {
        "background_command_failed"
    };
    if ongoing || force_handle || status == "timeout" {
        if status == "timeout" {
            payload["message"] = json!("Command timed out");
        } else if matches!(status.as_str(), "stopping" | "unknown") {
            payload["message"] =
                json!("Process-tree termination is unconfirmed; check the session again.");
        }
        let full_json_bytes = payload.get("output_json_bytes").and_then(Value::as_u64);
        if let Some(object) = payload.as_object_mut() {
            object.remove("command");
            object.remove("output_json_bytes");
        }
        if truncated {
            let Some(full_json_bytes) = full_json_bytes else {
                return tool_error_with_code("captured output size unavailable", "command_failed");
            };
            let mut output = payload["output"].as_str().unwrap_or_default().to_string();
            while full_json_bytes < json!(output).to_string().len() as u64 {
                let Some(marker) = output.find("\n... output omitted; full text in artifact ...\n")
                else {
                    break;
                };
                let Some((previous, _)) = output[..marker].char_indices().last() else {
                    break;
                };
                output.drain(previous..marker);
            }
            payload["output_visible_bytes"] = json!(output.len());
            payload["output"] = json!(output);
            result.content = payload.to_string();
            let visible = result.content.len() as u64;
            result.visible_bytes = Some(visible);
            result.original_bytes =
                Some(visible - json!(output).to_string().len() as u64 + full_json_bytes);
        } else {
            result.content = payload.to_string();
        }
        result.metadata = metadata;
        result.status = if success {
            ToolResultStatus::Success
        } else {
            ToolResultStatus::Error
        };
        result.error_code = (!success).then(|| code.to_string());
        return result;
    }
    let mut content = payload
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if !success && content.is_empty() {
        content = if status == "timeout" {
            "Command timed out".to_string()
        } else {
            exit_code.map_or_else(
                || "Process exit is unconfirmed".to_string(),
                |exit_code| format!("command exited with code {exit_code}"),
            )
        };
    }
    if let Some(cwd) = foreground_cwd {
        metadata.retain(|key, _| matches!(key.as_str(), "exit_code" | "shell"));
        metadata.insert("cwd".to_string(), json!(cwd));
        if !success {
            metadata.insert("error_code".to_string(), json!(code));
        }
    }
    result.content = content;
    result.status = if success {
        ToolResultStatus::Success
    } else {
        ToolResultStatus::Error
    };
    result.error_code = (!success).then(|| code.to_string());
    result.metadata = metadata;
    if truncated {
        result.original_bytes = payload.get("output_original_bytes").and_then(Value::as_u64);
        result.visible_bytes = Some(result.content.len() as u64);
    }
    result
}

fn management_error(
    message: impl Into<String>,
    code: &str,
    mut metadata: Metadata,
) -> ToolExecutionResult {
    let mut result = tool_error_with_code(message, code);
    let mut body: Value = serde_json::from_str(&result.content).expect("built-in error JSON");
    for key in ["status", "session_id"] {
        if let Some(value) = metadata.get(key) {
            body[key] = value.clone();
        }
    }
    metadata.insert("error_code".to_string(), json!(code));
    result.content = body.to_string();
    result.metadata = metadata;
    result
}

fn background_metadata(payload: &Value) -> Metadata {
    [
        "status",
        "session_id",
        "elapsed_seconds",
        "exit_code",
        "shell",
    ]
    .into_iter()
    .filter_map(|key| {
        payload
            .get(key)
            .filter(|value| !value.is_null())
            .map(|value| (key.to_string(), value.clone()))
    })
    .collect()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::runtime::background_sessions::BackgroundSessionAdoptOptions;
    use crate::runtime::processes::start_captured_process;
    use crate::workspace::{LocalWorkspaceBackend, WorkspaceBackend};

    #[test]
    fn immediate_handle_retains_an_already_observed_exit_and_exact_large_output_receipt() {
        for exit_code in [0, 7] {
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("text.txt");
            std::fs::write(&source, "x".repeat(12001)).unwrap();
            let mut started = start_captured_process(
                &[
                    "sh".to_string(),
                    "-c".to_string(),
                    format!("cat text.txt; exit {exit_code}"),
                ],
                root.path(),
                None,
            )
            .unwrap();
            started.child.wait().unwrap();
            let backend = Arc::new(LocalWorkspaceBackend::new(root.path()));
            let id = background_session_manager().adopt_running_process_with_options(
                BackgroundSessionAdoptOptions::new(
                    "cat",
                    root.path(),
                    None,
                    started.child,
                    started.output_path,
                )
                .with_started_at(started.started_at)
                .with_owner("owner", root.path())
                .with_artifact_context(backend.clone(), "owner", "fast"),
            );
            let payload = background_session_manager().check_for_tool(
                &id,
                backend.clone(),
                "owner",
                "fast",
                root.path(),
            );
            let result = background_command_result(payload, Some(".".to_string()), true);
            let body: Value = serde_json::from_str(&result.content).unwrap();
            assert_eq!(body["session_id"], id);
            assert_eq!(
                body["status"],
                if exit_code == 0 {
                    "completed"
                } else {
                    "failed"
                }
            );
            assert_eq!(body["exit_code"], exit_code);
            assert_eq!(
                result.status,
                if exit_code == 0 {
                    ToolResultStatus::Success
                } else {
                    ToolResultStatus::Error
                }
            );
            assert_eq!(result.visible_bytes, Some(result.content.len() as u64));
            assert!(result.original_bytes >= result.visible_bytes);
            assert_eq!(
                backend.read_text(&result.artifact.unwrap().path).unwrap(),
                "x".repeat(12001)
            );
        }
    }
}
