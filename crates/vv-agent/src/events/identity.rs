use std::time::{SystemTime, UNIX_EPOCH};

use super::{CreatedAtWire, EventId, RunEvent};

pub(super) fn timestamp_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_micros() as f64 / 1_000_000.0)
        .unwrap_or_default()
}

impl RunEvent {
    pub(crate) fn with_observed_identity(
        mut self,
        event_id: impl Into<String>,
        created_at: f64,
    ) -> Result<Self, String> {
        if !created_at.is_finite() || created_at < 0.0 {
            return Err("created_at must be a finite non-negative number".to_string());
        }
        self.event_id = EventId::stable(event_id)?;
        self.created_at = created_at;
        self.created_at_wire = CreatedAtWire::default();
        Ok(self)
    }
}
