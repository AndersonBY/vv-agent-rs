use std::collections::BTreeSet;
use std::panic::{catch_unwind, AssertUnwindSafe};

use super::helpers::normalize_summary_output;
use super::{MemoryManager, MEMORY_SUMMARY_NAME};
use crate::memory::message_sanitizer::filter_empty_assistant_messages;
use crate::memory::token_utils::count_messages_tokens;
use crate::memory::{RuntimeMemoryCallback, RuntimeMemoryCallbackError};
use crate::types::{Message, MessageRole};
use serde_json::{json, Value};

pub(super) struct SummaryParts {
    pub systems: Vec<Message>,
    pub previous: Vec<Message>,
    pub prefix: Vec<Message>,
    pub tail: Vec<Message>,
    pub cut: usize,
}

pub(super) fn summary_parts(messages: &[Message], keep: usize) -> Option<SummaryParts> {
    let mut blocks = Vec::new();
    let mut index = 0;
    while index < messages.len() {
        let message = &messages[index];
        if message.role == MessageRole::Tool {
            return None;
        }
        let mut end = index + 1;
        if !message.tool_calls.is_empty() {
            if message.role != MessageRole::Assistant {
                return None;
            }
            let mut ids = BTreeSet::new();
            for (offset, call) in message.tool_calls.iter().enumerate() {
                if call.id.is_empty() || !ids.insert(&call.id) {
                    return None;
                }
                if let Some(result) = messages.get(index + 1 + offset) {
                    if result.role != MessageRole::Tool
                        || result.tool_call_id.as_deref() != Some(&call.id)
                    {
                        return None;
                    }
                }
            }
            end += message.tool_calls.len();
        }
        blocks.push((index, end));
        index = end;
    }
    let raw: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter_map(|(i, m)| {
            (m.role != MessageRole::System
                && m.name.as_deref() != Some(MEMORY_SUMMARY_NAME)
                && !filter_empty_assistant_messages(std::slice::from_ref(m)).is_empty())
            .then_some(i)
        })
        .collect();
    let mut cut = raw
        .get(raw.len().saturating_sub(keep.max(1)))
        .copied()
        .unwrap_or(0);
    for (start, end) in blocks {
        if start < cut && cut < end {
            cut = start;
        }
    }
    Some(SummaryParts {
        systems: messages
            .iter()
            .filter(|m| {
                m.role == MessageRole::System && m.name.as_deref() != Some(MEMORY_SUMMARY_NAME)
            })
            .cloned()
            .collect(),
        previous: messages
            .iter()
            .filter(|m| m.name.as_deref() == Some(MEMORY_SUMMARY_NAME))
            .cloned()
            .collect(),
        prefix: raw
            .iter()
            .filter(|i| **i < cut)
            .map(|i| messages[*i].clone())
            .collect(),
        tail: raw
            .iter()
            .filter(|i| **i >= cut)
            .map(|i| messages[*i].clone())
            .collect(),
        cut,
    })
}

impl MemoryManager {
    pub(super) fn compress_memory(
        &self,
        messages: &[Message],
        cycle: Option<u32>,
        callback: Option<&RuntimeMemoryCallback>,
    ) -> Result<(Vec<Message>, bool), RuntimeMemoryCallbackError> {
        self.summarize_prefix(messages, self.config.keep_recent_messages, cycle, callback)
    }

    pub(super) fn summarize_prefix(
        &self,
        messages: &[Message],
        keep: usize,
        cycle: Option<u32>,
        callback: Option<&RuntimeMemoryCallback>,
    ) -> Result<(Vec<Message>, bool), RuntimeMemoryCallbackError> {
        let unchanged = || (messages.to_vec(), false);
        let Some(parts) = summary_parts(messages, keep) else {
            return Ok(unchanged());
        };
        if parts.prefix.is_empty() || (callback.is_none() && self.config.summary_callback.is_none())
        {
            return Ok(unchanged());
        }
        let prompt = super::prompts::build_compress_memory_prompt(
            &self.config.language,
            self.config.summary_event_limit,
            &parts.previous,
            &parts.prefix,
        );
        let raw = if let (Some(callback), Some(cycle)) = (callback, cycle) {
            callback(
                &prompt,
                self.config.summary_backend.as_deref(),
                self.config.summary_model.as_deref(),
                cycle,
            )?
        } else if let Some(callback) = &self.config.summary_callback {
            catch_unwind(AssertUnwindSafe(|| {
                callback(
                    &prompt,
                    self.config.summary_backend.as_deref(),
                    self.config.summary_model.as_deref(),
                )
            }))
            .ok()
            .flatten()
        } else {
            None
        };
        let mut summary = normalize_summary(
            &parse_first_json_object(&normalize_summary_output(&raw.unwrap_or_default()))
                .unwrap_or(Value::Null),
        );
        if !summary.as_object().unwrap().iter().any(|(k, v)| {
            k != "summary_version"
                && k != "user_constraints"
                && (v.as_str().is_some_and(|s| !s.is_empty())
                    || v.as_array().is_some_and(|a| !a.is_empty()))
        }) {
            return Ok(unchanged());
        }
        let Ok(evidence) = super::evidence::collect(&parts.previous, &parts.prefix) else {
            return Ok(unchanged());
        };
        if super::evidence::has_references(&evidence) && !self.recovery_tool_available {
            return Ok(unchanged());
        }
        let paths = summary["files_examined_or_modified"]
            .as_array_mut()
            .unwrap();
        let mut seen: BTreeSet<String> = paths
            .iter()
            .map(|p| p["path"].as_str().unwrap().to_owned())
            .collect();
        for previous in &parts.previous {
            if let Some((_, rest)) = previous.content.split_once("<Compressed Agent Memory>") {
                if let Some((body, _)) = rest.split_once("</Compressed Agent Memory>") {
                    let prior =
                        normalize_summary(&parse_first_json_object(body).unwrap_or(Value::Null));
                    for path in prior["files_examined_or_modified"].as_array().unwrap() {
                        if seen.insert(path["path"].as_str().unwrap().to_owned()) {
                            paths.push(path.clone());
                        }
                    }
                }
            }
        }
        for path in crate::memory::summary::collect_prefix_file_actions(&parts.prefix) {
            if seen.insert(path.path.clone()) {
                paths.push(serde_json::to_value(path).unwrap());
            }
        }
        let originals = summary["original_user_messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n\n");
        let content=format!("<Original User Request>\n{originals}\n</Original User Request>\n\n<Compressed Agent Memory>\n{}\n</Compressed Agent Memory>\n\n{}",jcs(&summary),super::evidence::render(&evidence));
        let mut summary_message = Message::user(content);
        summary_message.name = Some(MEMORY_SUMMARY_NAME.into());
        summary_message
            .metadata
            .insert(crate::types::COMPACTION_METADATA_KEY.into(), evidence);
        let mut candidate = parts.systems;
        candidate.push(summary_message);
        candidate.extend(parts.tail);
        let tokens = count_messages_tokens(&candidate, &self.config.model);
        if tokens >= count_messages_tokens(messages, &self.config.model)
            || tokens > self.effective_context_window()
        {
            return Ok(unchanged());
        }
        Ok((candidate, true))
    }
}

pub(super) fn jcs(value: &Value) -> String {
    serde_json_canonicalizer::to_string(value).expect("valid JSON value")
}

fn parse_first_json_object(raw: &str) -> Option<Value> {
    raw.char_indices()
        .filter(|(_, c)| *c == '{')
        .find_map(|(i, _)| {
            serde_json::Deserializer::from_str(&raw[i..])
                .into_iter::<Value>()
                .next()
                .and_then(Result::ok)
                .filter(Value::is_object)
        })
}
fn normalize_summary(payload: &Value) -> Value {
    let mut result = json!({"summary_version":"2.0"});
    for key in [
        "original_user_messages",
        "user_constraints",
        "decisions",
        "progress",
        "key_facts",
        "open_issues",
        "next_steps",
    ] {
        result[key] = payload[key]
            .as_array()
            .filter(|v| v.iter().all(Value::is_string))
            .map(|v| Value::Array(v.clone()))
            .unwrap_or(json!([]));
    }
    result["current_work_state"] = json!(payload["current_work_state"].as_str().unwrap_or(""));
    for (key, fields) in [
        ("files_examined_or_modified", ["path", "action", "summary"]),
        ("errors_and_fixes", ["error", "fix", "file"]),
    ] {
        let mut records = Vec::new();
        for item in payload[key].as_array().into_iter().flatten() {
            if !item.is_object() {
                continue;
            }
            if key == "files_examined_or_modified" {
                if item["path"].as_str().is_none_or(|s| s.trim().is_empty())
                    || !matches!(
                        item["action"].as_str(),
                        Some("read" | "created" | "modified" | "deleted")
                    )
                {
                    continue;
                }
            } else if !item["error"].is_string() {
                continue;
            }
            let mut record = json!({});
            for field in fields {
                record[field] = json!(item[field].as_str().unwrap_or(""));
            }
            records.push(record);
        }
        result[key] = Value::Array(records);
    }
    result
}
