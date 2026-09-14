use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::*;
use crate::types::{AgentResult, ModelCallStatus, TaskTokenUsageTotals};

fn history_error(message: impl Into<String>) -> CheckpointError {
    CheckpointError::new("checkpoint_history_invalid", message)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviousAgentInput {
    pub cycle_index: u32,
    #[serde(deserialize_with = "required_option")]
    pub input_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointHistory {
    pub sequence: u64,
    pub cycle_count: u64,
    pub model_call_count: u64,
    #[serde(deserialize_with = "required_option")]
    pub head_digest: Option<String>,
    pub usage: TaskTokenUsageTotals,
    #[serde(deserialize_with = "required_option")]
    pub previous_agent_input: Option<PreviousAgentInput>,
}

impl CheckpointHistory {
    pub fn validate(&self) -> CheckpointResult<()> {
        if [self.sequence, self.cycle_count, self.model_call_count]
            .into_iter()
            .any(|value| value > MAX_WIRE_INTEGER)
            || (self.sequence == 0) != self.head_digest.is_none()
        {
            return Err(history_error("history cursor is malformed"));
        }
        if let Some(digest) = &self.head_digest {
            validate_sha256(digest, "history.head_digest")?;
        }
        if self.sequence == 0 && self != &Self::default() {
            return Err(history_error(
                "empty history must have the default aggregate",
            ));
        }
        if self.sequence > 0 && self.cycle_count + self.model_call_count == 0 {
            return Err(history_error("history batch cannot be empty"));
        }
        if self.usage.cache_usage.source.as_deref() != Some("aggregate") {
            return Err(history_error("history cache usage must be aggregate"));
        }
        for value in [
            self.usage.input_tokens,
            self.usage.output_tokens,
            self.usage.total_tokens,
            self.usage.reasoning_tokens,
            self.previous_agent_input
                .as_ref()
                .and_then(|value| value.input_tokens),
        ] {
            if value.is_some_and(|value| value > MAX_WIRE_INTEGER) {
                return Err(history_error(
                    "history usage exceeds JSON-safe integer range",
                ));
            }
        }
        if self
            .previous_agent_input
            .as_ref()
            .is_some_and(|value| value.cycle_index == 0)
        {
            return Err(history_error("previous agent input cycle must be positive"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CheckpointHistoryRecords {
    pub cycles: Vec<CycleRecord>,
    pub model_calls: Vec<ModelCallRecord>,
    pub frontier: CheckpointHistory,
}

#[derive(Debug, Clone)]
pub struct CheckpointHistoryBatch {
    pub payload: Value,
    pub payload_digest: String,
    pub sequence: u64,
}

/// Move immutable completed history out of the active snapshot. Call only after
/// ownership and revision checks, inside the store's existing atomic write.
pub fn normalize_checkpoint_history(
    checkpoint: &mut Checkpoint,
) -> CheckpointResult<Option<CheckpointHistoryBatch>> {
    let retained = if checkpoint.terminal_result.is_some() {
        checkpoint.cycles.last().map(|cycle| u64::from(cycle.index))
    } else {
        checkpoint
            .cycles
            .iter()
            .map(|cycle| u64::from(cycle.index))
            .filter(|index| *index <= checkpoint.cycle_index)
            .max()
    };
    let Some(retained) = retained else {
        return Ok(None);
    };
    let mut cycles = Vec::new();
    checkpoint.cycles.retain(|cycle| {
        if u64::from(cycle.index) < retained {
            cycles.push(cycle.clone());
            false
        } else {
            true
        }
    });
    let journal_ids: std::collections::BTreeSet<_> = checkpoint
        .model_call_journal
        .iter()
        .filter_map(|entry| entry.call_id.as_deref())
        .collect();
    let mut model_calls = Vec::new();
    checkpoint.model_calls.retain(|record| {
        if u64::from(record.cycle_index) < retained
            && !journal_ids.contains(record.call_id.as_str())
        {
            model_calls.push(record.clone());
            false
        } else {
            true
        }
    });
    if cycles.is_empty() && model_calls.is_empty() {
        return Ok(None);
    }
    let sequence = checkpoint
        .history
        .sequence
        .checked_add(1)
        .filter(|value| *value <= MAX_WIRE_INTEGER)
        .ok_or_else(|| history_error("history sequence overflow"))?;
    let payload = serde_json::json!({
        "schema_version": "vv-agent.checkpoint-history.v1",
        "checkpoint_key": checkpoint.checkpoint_key,
        "sequence": sequence,
        "previous_digest": checkpoint.history.head_digest,
        "cycles": cycles.iter().map(CycleRecord::to_dict).collect::<Vec<_>>(),
        "model_calls": model_calls,
    });
    let payload_digest = format!(
        "{:x}",
        Sha256::digest(canonical_json_bytes(&payload, "checkpoint history")?)
    );
    checkpoint.history.cycle_count += cycles.len() as u64;
    for record in &model_calls {
        checkpoint
            .history
            .usage
            .append(record, checkpoint.history.model_call_count == 0);
        checkpoint.history.model_call_count += 1;
        if record.operation == ModelCallOperation::AgentCycle
            && record.status == ModelCallStatus::Completed
        {
            checkpoint.history.previous_agent_input = Some(PreviousAgentInput {
                cycle_index: record.cycle_index,
                input_tokens: record.usage.input_tokens,
            });
        }
    }
    checkpoint.history.sequence = sequence;
    checkpoint.history.head_digest = Some(payload_digest.clone());
    if let Some(value) = &checkpoint.terminal_result {
        let mut result = AgentResult::from_dict(value).map_err(history_error)?;
        result.cycles = checkpoint.cycles.clone();
        result.token_usage =
            crate::runtime::token_usage::summarize_task_token_usage(&checkpoint.model_calls);
        checkpoint.terminal_result = Some(result.to_dict());
    }
    checkpoint.validate()?;
    Ok(Some(CheckpointHistoryBatch {
        payload,
        payload_digest,
        sequence,
    }))
}

pub fn decode_checkpoint_history(
    checkpoint: &Checkpoint,
    payloads: &[String],
) -> CheckpointResult<CheckpointHistoryRecords> {
    if payloads.len() as u64 != checkpoint.history.sequence {
        return Err(history_error("history archive is incomplete"));
    }
    let mut records = CheckpointHistoryRecords::default();
    let mut previous: Option<String> = None;
    let mut aggregate = CheckpointHistory::default();
    for (index, raw) in payloads.iter().enumerate() {
        let payload = crate::runtime::checkpoint_codec::strict_json_value(raw)
            .map_err(|error| history_error(error.to_string()))?;
        let object = payload
            .as_object()
            .ok_or_else(|| history_error("history batch must be an object"))?;
        let fields = [
            "schema_version",
            "checkpoint_key",
            "sequence",
            "previous_digest",
            "cycles",
            "model_calls",
        ];
        if object.len() != fields.len()
            || fields.iter().any(|field| !object.contains_key(*field))
            || payload["schema_version"] != "vv-agent.checkpoint-history.v1"
            || payload["checkpoint_key"] != checkpoint.checkpoint_key
            || payload["sequence"].as_u64() != Some(index as u64 + 1)
            || payload["previous_digest"]
                != serde_json::to_value(&previous).expect("optional string")
        {
            return Err(history_error("history batch identity or chain is invalid"));
        }
        for value in payload["cycles"]
            .as_array()
            .ok_or_else(|| history_error("history cycles must be an array"))?
        {
            let cycle = CycleRecord::from_dict(value).map_err(history_error)?;
            if records
                .cycles
                .last()
                .is_some_and(|existing| existing.index >= cycle.index)
            {
                return Err(history_error("duplicate or unordered historical cycle"));
            }
            records.cycles.push(cycle);
        }
        let calls: Vec<ModelCallRecord> = serde_json::from_value(payload["model_calls"].clone())
            .map_err(|error| history_error(error.to_string()))?;
        if calls.is_empty() && payload["cycles"].as_array().is_some_and(Vec::is_empty) {
            return Err(history_error("empty history batch"));
        }
        for record in calls {
            if records
                .model_calls
                .iter()
                .any(|existing| existing.call_id == record.call_id)
            {
                return Err(history_error("duplicate historical model call"));
            }
            aggregate
                .usage
                .append(&record, aggregate.model_call_count == 0);
            aggregate.model_call_count += 1;
            if record.operation == ModelCallOperation::AgentCycle
                && record.status == ModelCallStatus::Completed
            {
                aggregate.previous_agent_input = Some(PreviousAgentInput {
                    cycle_index: record.cycle_index,
                    input_tokens: record.usage.input_tokens,
                });
            }
            records.model_calls.push(record);
        }
        previous = Some(format!(
            "{:x}",
            Sha256::digest(canonical_json_bytes(&payload, "checkpoint history")?)
        ));
    }
    aggregate.sequence = payloads.len() as u64;
    aggregate.cycle_count = records.cycles.len() as u64;
    aggregate.head_digest = previous;
    if aggregate != *checkpoint.history {
        return Err(history_error("history aggregate or digest mismatch"));
    }
    records.frontier = aggregate;
    if records.cycles.iter().any(|archived| {
        checkpoint
            .cycles
            .iter()
            .any(|active| active.index == archived.index)
    }) || records.model_calls.iter().any(|archived| {
        checkpoint
            .model_calls
            .iter()
            .any(|active| active.call_id == archived.call_id)
    }) {
        return Err(history_error("archive overlaps the active checkpoint tail"));
    }
    Ok(records)
}

pub fn hydrate_checkpoint_result(
    store: &dyn CheckpointStore,
    checkpoint: &Checkpoint,
    mut result: AgentResult,
) -> CheckpointResult<AgentResult> {
    if checkpoint.history.sequence == 0 {
        return Ok(result);
    }
    let prefix = store.load_checkpoint_history(&checkpoint.checkpoint_key)?;
    if prefix.frontier != *checkpoint.history {
        return Err(CheckpointError::new(
            "checkpoint_history_changed",
            "history changed during result hydration",
        ));
    }
    let mut cycles = prefix.cycles;
    for cycle in result.cycles {
        if let Some(existing) = cycles.iter().find(|existing| existing.index == cycle.index) {
            if existing != &cycle {
                return Err(history_error("result conflicts with archived cycle"));
            }
        } else {
            cycles.push(cycle);
        }
    }
    let mut calls = prefix.model_calls;
    for call in &checkpoint.model_calls {
        if calls
            .iter()
            .any(|existing| existing.call_id == call.call_id)
        {
            return Err(history_error(
                "model call appears in both history and active tail",
            ));
        }
        calls.push(call.clone());
    }
    result.cycles = cycles;
    result.token_usage = crate::runtime::token_usage::summarize_task_token_usage(&calls);
    Ok(result)
}

fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}
