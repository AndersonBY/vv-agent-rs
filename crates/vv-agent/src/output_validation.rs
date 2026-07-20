use std::sync::Arc;

use crate::model::ModelRef;
use crate::model_settings::ModelSettings;

pub const OUTPUT_VALIDATION_FAILED: &str = "output_validation_failed";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputValidationContext {
    pub run_id: String,
    pub agent_name: String,
    pub output_type_name: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputValidationResult {
    pub valid: bool,
    pub code: Option<String>,
    pub message: Option<String>,
}

impl OutputValidationResult {
    pub fn accept() -> Self {
        Self {
            valid: true,
            code: None,
            message: None,
        }
    }

    pub fn reject(code: impl Into<String>, message: Option<impl Into<String>>) -> Self {
        Self {
            valid: false,
            code: Some(code.into()),
            message: message.map(Into::into),
        }
    }

    pub fn reject_code(code: impl Into<String>) -> Self {
        Self::reject(code, None::<String>)
    }

    pub(crate) fn normalized(self) -> Self {
        if self.valid {
            if self.code.is_none() && self.message.is_none() {
                return self;
            }
            return Self::reject(
                "output_validator_contract_invalid",
                Some("a valid output result cannot contain an error"),
            );
        }
        if self.code.as_deref().is_none_or(str::is_empty)
            || self
                .code
                .as_deref()
                .is_some_and(|code| code.trim().is_empty())
        {
            return Self::reject(
                "output_validator_contract_invalid",
                Some("an invalid output result requires a non-empty code"),
            );
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OutputRepairRequest {
    pub invalid_output: String,
    pub validation_code: String,
    pub validation_message: Option<String>,
    pub model: Option<ModelRef>,
    pub model_settings: Option<ModelSettings>,
    pub tools: Vec<serde_json::Value>,
}

pub type HostOutputValidator =
    Arc<dyn Fn(&str, &OutputValidationContext) -> OutputValidationResult + Send + Sync>;
pub type OutputRepair = Arc<dyn Fn(&OutputRepairRequest) -> Result<String, String> + Send + Sync>;
