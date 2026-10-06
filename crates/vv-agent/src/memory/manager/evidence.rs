use super::compaction::jcs;
use crate::types::{
    validate_compaction_metadata, Message, MessageRole, ToolExecutionResult,
    COMPACTION_METADATA_KEY,
};
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub(super) fn has_references(evidence: &Value) -> bool {
    ["artifacts", "cursors"]
        .iter()
        .any(|k| evidence[k].as_array().is_some_and(|a| !a.is_empty()))
}
pub(super) fn collect(previous: &[Message], prefix: &[Message]) -> Result<Value, String> {
    let mut evidence = json!({"artifacts":[],"cursors":[]});
    let mut seen = [BTreeSet::new(), BTreeSet::new()];
    let mut merge = |manifest: Value| -> Result<(), String> {
        validate_compaction_metadata(
            &[(COMPACTION_METADATA_KEY.to_string(), manifest.clone())]
                .into_iter()
                .collect(),
        )?;
        for (index, key) in ["artifacts", "cursors"].iter().enumerate() {
            for record in manifest[key].as_array().unwrap() {
                if seen[index].insert(jcs(record)) {
                    evidence[key].as_array_mut().unwrap().push(record.clone());
                }
            }
        }
        Ok(())
    };
    for message in previous {
        if let Some(manifest) = message.metadata.get(COMPACTION_METADATA_KEY) {
            merge(manifest.clone())?;
        }
    }
    let mut calls = &[][..];
    for message in prefix {
        if message.role == MessageRole::Assistant {
            calls = message.tool_calls.as_slice();
        }
        if message.role != MessageRole::Tool {
            continue;
        }
        let call = calls
            .iter()
            .find(|c| Some(c.id.as_str()) == message.tool_call_id.as_deref())
            .ok_or("missing tool call")?;
        let common = json!({"tool_call_id":call.id,"tool_name":call.name,"arguments":jcs(&serde_json::to_value(&call.arguments).map_err(|e|e.to_string())?)});
        let mut refs = json!({"artifacts":[],"cursors":[]});
        if let Some(artifact) = &message.artifact_ref {
            let mut record = common.clone();
            record["artifact_ref"] = serde_json::to_value(artifact).map_err(|e| e.to_string())?;
            refs["artifacts"].as_array_mut().unwrap().push(record);
        }
        if crate::memory::artifacts::has_recovery_envelope(&message.content) {
            let (body, raw) = message
                .content
                .trim_end()
                .rsplit_once('\n')
                .ok_or("missing recovery envelope")?;
            let envelope: Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
            if envelope.as_object().is_none_or(|o| o.len() != 1) {
                return Err("invalid recovery envelope".into());
            }
            let recovery = envelope["vv_agent_recovery"]
                .as_object()
                .ok_or("invalid recovery object")?;
            if recovery.keys().any(|k| {
                ![
                    "truncated",
                    "truncation_reason",
                    "original_bytes",
                    "visible_bytes",
                    "artifact",
                    "cursor",
                ]
                .contains(&k.as_str())
            }) {
                return Err("invalid recovery field".into());
            }
            let mut wire = json!({"tool_call_id":call.id,"content":body,"status_code":"SUCCESS","directive":"continue"});
            wire.as_object_mut().unwrap().extend(recovery.clone());
            let result = ToolExecutionResult::from_dict(&wire)?;
            if let Some(artifact) = result.artifact {
                let mut record = common.clone();
                record["artifact_ref"] = serde_json::to_value(artifact).unwrap();
                refs["artifacts"].as_array_mut().unwrap().push(record);
            }
            if let Some(cursor) = result.cursor {
                let mut record = common.clone();
                record["cursor"] = serde_json::to_value(cursor).unwrap();
                refs["cursors"].as_array_mut().unwrap().push(record);
            }
        }
        merge(refs)?;
    }
    Ok(evidence)
}
pub(super) fn render(evidence: &Value) -> String {
    let mut lines = vec!["<Persisted Artifacts>".to_string()];
    for (key, pointer) in [("artifacts", "artifact_ref"), ("cursors", "cursor")] {
        for record in evidence[key].as_array().unwrap() {
            let mut visible = json!({"tool_call_id":record["tool_call_id"],"tool_name":record["tool_name"],"arguments":record["arguments"]});
            if key == "artifacts" {
                visible["artifact_path"] = record[pointer]["path"].clone();
                visible["retrieval_hint"] = json!("use read_file on artifact_path if needed");
            } else {
                visible["path"] = record[pointer]["path"].clone();
                visible["offset_chars"] = record[pointer]["offset_chars"].clone();
                visible["retrieval_hint"] = json!("use read_file on path if needed");
            }
            lines.push(format!("- {}", jcs(&visible)));
        }
    }
    lines.push("</Persisted Artifacts>".into());
    lines.join("\n")
}
