use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::distributed::DistributedCapabilities;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeRecipe {
    pub settings_file: String,
    pub backend: String,
    pub model: String,
    pub workspace: String,
    pub timeout_seconds: f64,
    pub log_preview_chars: Option<usize>,
    pub capabilities: DistributedCapabilities,
}

impl RuntimeRecipe {
    pub fn new(
        settings_file: impl Into<String>,
        backend: impl Into<String>,
        model: impl Into<String>,
        workspace: impl Into<String>,
    ) -> Self {
        Self {
            settings_file: settings_file.into(),
            backend: backend.into(),
            model: model.into(),
            workspace: workspace.into(),
            timeout_seconds: 90.0,
            log_preview_chars: None,
            capabilities: DistributedCapabilities::default(),
        }
    }

    pub fn to_dict(&self) -> Value {
        serde_json::json!({
            "settings_file": self.settings_file,
            "backend": self.backend,
            "model": self.model,
            "workspace": self.workspace,
            "timeout_seconds": self.timeout_seconds,
            "log_preview_chars": self.log_preview_chars,
            "capabilities": self.capabilities.to_dict(),
        })
    }

    pub fn from_dict(data: &Value) -> Result<Self, String> {
        let object = data
            .as_object()
            .ok_or_else(|| "RuntimeRecipe payload must be an object".to_string())?;
        Ok(Self {
            settings_file: read_required_string(object, "settings_file")?.to_string(),
            backend: read_required_string(object, "backend")?.to_string(),
            model: read_required_string(object, "model")?.to_string(),
            workspace: read_required_string(object, "workspace")?.to_string(),
            timeout_seconds: object
                .get("timeout_seconds")
                .and_then(Value::as_f64)
                .unwrap_or(90.0),
            log_preview_chars: object
                .get("log_preview_chars")
                .filter(|value| !value.is_null())
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok()),
            capabilities: DistributedCapabilities::from_dict(
                object
                    .get("capabilities")
                    .ok_or_else(|| "capabilities must be an object".to_string())?,
            )?,
        })
    }

    pub fn validate(&self) -> Result<(), String> {
        for (field_name, value) in [
            ("settings_file", self.settings_file.as_str()),
            ("backend", self.backend.as_str()),
            ("model", self.model.as_str()),
            ("workspace", self.workspace.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(format!(
                    "runtime_recipe.{field_name} must be a non-empty string"
                ));
            }
        }
        if !self.timeout_seconds.is_finite() || self.timeout_seconds <= 0.0 {
            return Err(
                "runtime_recipe.timeout_seconds must be a finite positive number".to_string(),
            );
        }
        self.capabilities.validate()
    }
}

fn read_required_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing required string field {key:?}"))
}
