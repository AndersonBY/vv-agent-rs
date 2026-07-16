use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use serde_json::Value;

use crate::types::TaskTokenUsage;

#[derive(Default)]
pub(super) struct ObservedRunProgress {
    completed_cycles: AtomicU32,
    token_usage: Mutex<Option<TaskTokenUsage>>,
}

impl ObservedRunProgress {
    pub(super) fn record_completed_cycle(&self, payload: &BTreeMap<String, Value>) {
        let cycle_index = payload
            .get("cycle")
            .and_then(Value::as_u64)
            .unwrap_or_default() as u32;
        self.completed_cycles
            .fetch_max(cycle_index, Ordering::Relaxed);

        let Some(raw_usage) = payload.get("token_usage") else {
            return;
        };
        let Ok(usage) = serde_json::from_value(raw_usage.clone()) else {
            return;
        };
        if let Ok(mut observed) = self.token_usage.lock() {
            observed
                .get_or_insert_with(TaskTokenUsage::default)
                .add_cycle(cycle_index, usage);
        }
    }

    pub(super) fn token_usage(&self) -> Option<TaskTokenUsage> {
        self.token_usage.lock().ok().and_then(|usage| usage.clone())
    }

    pub(super) fn snapshot(&self) -> (u32, Option<TaskTokenUsage>) {
        (
            self.completed_cycles.load(Ordering::Relaxed),
            self.token_usage(),
        )
    }
}
