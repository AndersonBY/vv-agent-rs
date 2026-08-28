use super::*;

impl CheckpointResumeController {
    pub(crate) fn refresh_authoritative(&mut self) -> CheckpointResult<Checkpoint> {
        self.reload()?;
        self.validate_existing_definition(self.require_checkpoint()?)?;
        Ok(self.require_checkpoint()?.clone())
    }
}
