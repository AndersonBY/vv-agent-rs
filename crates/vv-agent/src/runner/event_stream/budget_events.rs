use std::collections::BTreeMap;

use serde_json::Value;

use crate::events::{RunEvent, RunEventPayload};

use super::RuntimeEventContext;

pub(super) fn map_budget_snapshot(
    payload: &BTreeMap<String, Value>,
    context: &RuntimeEventContext,
) -> Option<RunEvent> {
    Some(RunEvent::new(
        &context.run_id,
        &context.trace_id,
        &context.agent_name,
        cycle_index(payload),
        RunEventPayload::BudgetSnapshot {
            enforcement_boundary: serde_json::from_value(
                payload.get("enforcement_boundary")?.clone(),
            )
            .ok()?,
            budget_usage: serde_json::from_value(payload.get("budget_usage")?.clone()).ok()?,
        },
    ))
}

pub(super) fn map_budget_exhausted(
    payload: &BTreeMap<String, Value>,
    context: &RuntimeEventContext,
) -> Option<RunEvent> {
    Some(RunEvent::new(
        &context.run_id,
        &context.trace_id,
        &context.agent_name,
        cycle_index(payload),
        RunEventPayload::BudgetExhausted {
            enforcement_boundary: serde_json::from_value(
                payload.get("enforcement_boundary")?.clone(),
            )
            .ok()?,
            budget_usage: serde_json::from_value(payload.get("budget_usage")?.clone()).ok()?,
            budget_exhaustion: serde_json::from_value(payload.get("budget_exhaustion")?.clone())
                .ok()?,
        },
    ))
}

fn cycle_index(payload: &BTreeMap<String, Value>) -> Option<u32> {
    payload
        .get("cycle")
        .and_then(Value::as_u64)
        .map(|cycle| cycle as u32)
}
