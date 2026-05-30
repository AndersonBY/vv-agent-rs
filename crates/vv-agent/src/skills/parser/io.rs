use std::path::Path;

use crate::skills::errors::SkillError;

pub(super) fn read_utf8_lossy(path: &Path) -> Result<String, SkillError> {
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
