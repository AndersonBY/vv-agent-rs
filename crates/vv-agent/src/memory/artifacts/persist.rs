use std::sync::Arc;

use crate::types::ToolArtifactRef;
use crate::workspace::{persist_text_artifact, WorkspaceBackend};

pub(super) fn persist_tool_content(
    content: &str,
    tool_call_id: Option<&str>,
    backend: &Arc<dyn WorkspaceBackend>,
    artifact_namespace: &str,
) -> Option<ToolArtifactRef> {
    persist_text_artifact(
        backend.clone(),
        artifact_namespace,
        tool_call_id.unwrap_or("tool-result"),
        content,
    )
    .ok()
}
