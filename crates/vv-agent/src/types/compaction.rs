use super::{Metadata, ToolArtifactRef, ToolResultCursor};
use serde_json::Value;

pub(crate) const COMPACTION_METADATA_KEY: &str = "_vv_agent_compaction";

pub(crate) fn validate_compaction_metadata(metadata: &Metadata) -> Result<(), String> {
    let Some(manifest) = metadata.get(COMPACTION_METADATA_KEY) else {
        return Ok(());
    };
    let object = manifest
        .as_object()
        .ok_or("invalid compaction evidence manifest")?;
    if object.len() != 2 || !object.contains_key("artifacts") || !object.contains_key("cursors") {
        return Err("invalid compaction evidence manifest fields".into());
    }
    for (key, pointer) in [("artifacts", "artifact_ref"), ("cursors", "cursor")] {
        for record in object[key]
            .as_array()
            .ok_or("compaction evidence must be arrays")?
        {
            let record = record
                .as_object()
                .ok_or("invalid compaction evidence record")?;
            if record.len() != 4
                || !["tool_call_id", "tool_name", "arguments", pointer]
                    .iter()
                    .all(|k| record.contains_key(*k))
            {
                return Err("invalid compaction evidence record fields".into());
            }
            for key in ["tool_call_id", "tool_name"] {
                if record[key].as_str().is_none_or(|s| s.is_empty()) {
                    return Err("invalid compaction evidence identity".into());
                }
            }
            let raw = record["arguments"]
                .as_str()
                .ok_or("invalid compaction evidence arguments")?;
            let arguments: Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
            if !arguments.is_object()
                || serde_json_canonicalizer::to_string(&arguments).map_err(|e| e.to_string())?
                    != raw
            {
                return Err("compaction evidence arguments must be canonical object JSON".into());
            }
            if key == "artifacts" {
                serde_json::from_value::<ToolArtifactRef>(record[pointer].clone())
                    .map_err(|e| e.to_string())?
                    .validate()?;
            } else {
                serde_json::from_value::<ToolResultCursor>(record[pointer].clone())
                    .map_err(|e| e.to_string())?
                    .validate()?;
            }
        }
    }
    Ok(())
}
