use super::{CreatedAtWire, EventId, RunEvent};

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
