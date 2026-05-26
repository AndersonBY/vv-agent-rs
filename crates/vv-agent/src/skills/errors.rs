#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SkillError {
    pub message: String,
}

impl SkillError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl From<SkillParseError> for SkillError {
    fn from(error: SkillParseError) -> Self {
        Self::new(error.to_string())
    }
}

impl From<SkillValidationError> for SkillError {
    fn from(error: SkillValidationError) -> Self {
        Self::new(error.to_string())
    }
}

impl From<std::io::Error> for SkillError {
    fn from(error: std::io::Error) -> Self {
        Self::new(error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SkillParseError {
    pub message: String,
}

impl SkillParseError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SkillValidationError {
    pub message: String,
}

impl SkillValidationError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
