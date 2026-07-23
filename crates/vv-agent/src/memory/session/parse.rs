use super::{SessionMemory, SessionMemoryEntry};
use crate::memory::SessionMemoryOutputInvalidReason;

impl SessionMemory {
    pub fn parse_extraction_result(&self, raw: &str, cycle: i32) -> Vec<SessionMemoryEntry> {
        self.parse_extraction_result_checked(raw, cycle)
            .unwrap_or_default()
    }

    pub(crate) fn parse_extraction_result_checked(
        &self,
        raw: &str,
        cycle: i32,
    ) -> Result<Vec<SessionMemoryEntry>, SessionMemoryOutputInvalidReason> {
        if raw.trim().is_empty() {
            return Err(SessionMemoryOutputInvalidReason::EmptyOutput);
        }
        let Some(array_text) = extract_first_json_array(raw) else {
            return Err(SessionMemoryOutputInvalidReason::JsonArrayMissing);
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(array_text) else {
            return Err(SessionMemoryOutputInvalidReason::JsonArrayMissing);
        };
        let Some(items) = value.as_array() else {
            return Err(SessionMemoryOutputInvalidReason::JsonArrayMissing);
        };
        let entries = items
            .iter()
            .filter_map(|item| {
                let object = item.as_object()?;
                let content = object.get("content")?.as_str()?.trim();
                if content.is_empty() {
                    return None;
                }
                let category = object
                    .get("category")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("key_fact");
                let importance = object
                    .get("importance")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(5)
                    .min(10) as u8;
                Some(SessionMemoryEntry::new(
                    category,
                    content,
                    cycle,
                    importance.max(1),
                ))
            })
            .collect::<Vec<_>>();
        if entries.is_empty() {
            Err(SessionMemoryOutputInvalidReason::NoValidEntries)
        } else {
            Ok(entries)
        }
    }
}

fn extract_first_json_array(raw: &str) -> Option<&str> {
    let start = raw.find('[')?;
    let mut depth = 0_i32;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in raw[start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '[' if !in_string => depth += 1,
            ']' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(&raw[start..start + offset + ch.len_utf8()]);
                }
            }
            _ => {}
        }
    }
    None
}
