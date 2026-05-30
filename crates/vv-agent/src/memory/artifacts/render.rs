#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedArtifact {
    pub path: String,
    pub tool_name: Option<String>,
    pub arguments: Option<String>,
}

pub fn render_persisted_artifacts_section(artifacts: &[PersistedArtifact]) -> Option<String> {
    if artifacts.is_empty() {
        return None;
    }
    let mut lines = vec!["<Persisted Artifacts>".to_string()];
    for artifact in artifacts {
        let tool = artifact.tool_name.as_deref().unwrap_or("unknown");
        let arguments = artifact.arguments.as_deref().unwrap_or("");
        let hint = "retrieval_hint: use read_file on artifact_path if needed";
        if arguments.is_empty() {
            lines.push(format!("- {} (tool: {tool}, {hint})", artifact.path));
        } else {
            lines.push(format!(
                "- {} (tool: {tool}, arguments: {arguments}, {hint})",
                artifact.path
            ));
        }
    }
    lines.push("</Persisted Artifacts>".to_string());
    Some(lines.join("\n"))
}
