use std::collections::BTreeMap;

use serde_json::Value;

#[derive(Debug, Default)]
pub(super) struct StreamingToolCallParts {
    pub(super) id: String,
    pub(super) has_provider_id: bool,
    pub(super) index: Option<usize>,
    pub(super) name: String,
    pub(super) arguments: String,
    pub(super) extra_content: Option<Value>,
}

pub(super) fn resolve_stream_tool_call_key(
    tool_call: &vv_llm::ToolCall,
    tool_calls: &BTreeMap<String, StreamingToolCallParts>,
) -> Option<String> {
    // IDs win when supplied. Index is the provider's stable stream identity,
    // not the position of this delta within its chunk.
    if !tool_call.id.is_empty() {
        if let Some((key, _)) = tool_calls
            .iter()
            .find(|(_, slot)| slot.has_provider_id && slot.id == tool_call.id)
        {
            return Some(key.clone());
        }
    }
    let candidate = if let Some(index) = tool_call.index {
        let mut matches = tool_calls
            .iter()
            .filter(|(_, slot)| slot.index == Some(index));
        let first = matches.next();
        if matches.next().is_some() {
            // Duplicate provider indices cannot safely identify a continuation.
            None
        } else {
            first
        }
    } else if tool_calls.len() == 1 && tool_call.name.is_empty() {
        tool_calls.iter().next()
    } else {
        None
    };
    if let Some((key, slot)) = candidate {
        if tool_call.id.is_empty() || !slot.has_provider_id {
            return Some(key.clone());
        }
    }
    if !tool_call.name.is_empty() {
        // Keep internal keys separate from provider IDs and unique across calls.
        return Some(format!("call_stream_{}", tool_calls.len()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delta(id: &str, index: Option<usize>, name: &str) -> vv_llm::ToolCall {
        vv_llm::ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: String::new(),
            index,
            extra_content: None,
        }
    }

    fn calls() -> BTreeMap<String, StreamingToolCallParts> {
        [("call_stream_0", "a", 0), ("call_stream_1", "b", 3)]
            .into_iter()
            .map(|(key, id, index)| {
                (
                    key.into(),
                    StreamingToolCallParts {
                        id: id.into(),
                        has_provider_id: true,
                        index: Some(index),
                        ..Default::default()
                    },
                )
            })
            .collect()
    }

    #[test]
    fn interleaved_idless_fragments_follow_provider_indices() {
        let calls = calls();
        for (index, expected) in [
            (0, "call_stream_0"),
            (3, "call_stream_1"),
            (0, "call_stream_0"),
        ] {
            assert_eq!(
                resolve_stream_tool_call_key(&delta("", Some(index), ""), &calls).as_deref(),
                Some(expected)
            );
        }
        assert_eq!(
            resolve_stream_tool_call_key(&delta("a", Some(3), ""), &calls).as_deref(),
            Some("call_stream_0")
        );
    }

    #[test]
    fn ambiguous_unidentified_fragments_are_not_assigned_to_last_call() {
        assert_eq!(
            resolve_stream_tool_call_key(&delta("", None, ""), &calls()),
            None
        );
        let mut one = calls();
        one.remove("call_stream_1");
        assert_eq!(
            resolve_stream_tool_call_key(&delta("", None, ""), &one).as_deref(),
            Some("call_stream_0")
        );
    }

    #[test]
    fn late_id_can_promote_index_only_slot_without_merging_distinct_ids() {
        let mut calls = calls();
        calls.get_mut("call_stream_0").unwrap().has_provider_id = false;
        assert_eq!(
            resolve_stream_tool_call_key(&delta("late-a", Some(0), ""), &calls).as_deref(),
            Some("call_stream_0")
        );
        assert_eq!(
            resolve_stream_tool_call_key(&delta("new-b", Some(3), "lookup"), &calls).as_deref(),
            Some("call_stream_2")
        );
    }
    #[test]
    fn provider_id_does_not_collide_with_synthetic_slot_id() {
        let mut calls = calls();
        let slot = calls.get_mut("call_stream_0").unwrap();
        slot.id = "call_stream_0".into();
        slot.has_provider_id = false;
        assert_eq!(
            resolve_stream_tool_call_key(&delta("call_stream_0", Some(9), "lookup"), &calls)
                .as_deref(),
            Some("call_stream_2")
        );
    }
    #[test]
    fn unidentified_starts_and_conflicting_ids_do_not_capture_existing_slots() {
        let calls = calls();
        assert_eq!(
            resolve_stream_tool_call_key(&delta("unknown", Some(0), ""), &calls),
            None
        );
        assert_eq!(
            resolve_stream_tool_call_key(&delta("", Some(99), ""), &calls),
            None
        );
        assert_eq!(
            resolve_stream_tool_call_key(&delta("b", Some(0), ""), &calls).as_deref(),
            Some("call_stream_1")
        );
        let mut one = calls;
        one.remove("call_stream_1");
        assert_eq!(
            resolve_stream_tool_call_key(&delta("", None, "lookup"), &one).as_deref(),
            Some("call_stream_1")
        );
        assert_eq!(
            resolve_stream_tool_call_key(&delta("", Some(0), "lookup"), &one).as_deref(),
            Some("call_stream_0")
        );
    }
    #[test]
    fn duplicate_indices_remain_ambiguous_but_explicit_ids_still_resolve() {
        let mut calls = calls();
        calls.get_mut("call_stream_1").unwrap().index = Some(0);
        assert_eq!(
            resolve_stream_tool_call_key(&delta("", Some(0), ""), &calls),
            None
        );
        assert_eq!(
            resolve_stream_tool_call_key(&delta("a", Some(0), ""), &calls).as_deref(),
            Some("call_stream_0")
        );
        assert_eq!(
            resolve_stream_tool_call_key(&delta("b", Some(0), ""), &calls).as_deref(),
            Some("call_stream_1")
        );
        assert_eq!(
            resolve_stream_tool_call_key(&delta("", Some(0), ""), &calls),
            None
        );
    }
}
