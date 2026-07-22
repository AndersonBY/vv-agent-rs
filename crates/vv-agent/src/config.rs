use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::Value;
use thiserror::Error;

use crate::types::AgentTask;

mod model_resolution;
mod settings_literal;

pub use model_resolution::{build_vv_llm_from_local_settings, resolve_model_endpoint};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EndpointConfig {
    pub endpoint_id: String,
    pub api_key: String,
    pub api_base: String,
    pub endpoint_type: String,
}

impl EndpointConfig {
    pub fn new(
        endpoint_id: impl Into<String>,
        api_key: impl Into<String>,
        api_base: impl Into<String>,
    ) -> Self {
        Self {
            endpoint_id: endpoint_id.into(),
            api_key: api_key.into(),
            api_base: api_base.into(),
            endpoint_type: "default".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EndpointOption {
    pub endpoint: EndpointConfig,
    pub model_id: String,
}

impl EndpointOption {
    pub fn new(endpoint: EndpointConfig, model_id: impl Into<String>) -> Self {
        Self {
            endpoint,
            model_id: model_id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResolvedModelConfig {
    pub backend: String,
    pub requested_model: String,
    pub selected_model: String,
    pub model_id: String,
    pub context_length: Option<u64>,
    pub max_output_tokens: Option<u64>,
    #[serde(default)]
    pub function_call_available: bool,
    #[serde(default)]
    pub response_format_available: bool,
    #[serde(default)]
    pub native_multimodal: bool,
    pub endpoint_options: Vec<EndpointOption>,
}

impl ResolvedModelConfig {
    pub fn new(
        backend: impl Into<String>,
        requested_model: impl Into<String>,
        selected_model: impl Into<String>,
        model_id: impl Into<String>,
        endpoint_options: Vec<EndpointOption>,
    ) -> Self {
        Self {
            backend: backend.into(),
            requested_model: requested_model.into(),
            selected_model: selected_model.into(),
            model_id: model_id.into(),
            context_length: None,
            max_output_tokens: None,
            function_call_available: false,
            response_format_available: false,
            native_multimodal: false,
            endpoint_options,
        }
    }

    pub fn with_token_limits(
        mut self,
        context_length: Option<u64>,
        max_output_tokens: Option<u64>,
    ) -> Self {
        self.context_length = context_length;
        self.max_output_tokens = max_output_tokens;
        self
    }

    pub fn with_capabilities(
        mut self,
        function_call_available: bool,
        response_format_available: bool,
        native_multimodal: bool,
    ) -> Self {
        self.function_call_available = function_call_available;
        self.response_format_available = response_format_available;
        self.native_multimodal = native_multimodal;
        self
    }

    pub fn endpoint(&self) -> Option<&EndpointConfig> {
        self.endpoint_options.first().map(|option| &option.endpoint)
    }
}

pub fn apply_resolved_model_limits(task: &mut AgentTask, resolved: &ResolvedModelConfig) {
    task.native_multimodal = resolved.native_multimodal;
    project_resolved_model_limits(
        &mut task.metadata,
        resolved.context_length,
        resolved.max_output_tokens,
    );
    task.metadata
        .entry("function_call_available".to_string())
        .or_insert_with(|| Value::Bool(resolved.function_call_available));
    task.metadata
        .entry("response_format_available".to_string())
        .or_insert_with(|| Value::Bool(resolved.response_format_available));
    task.metadata
        .entry("native_multimodal".to_string())
        .or_insert_with(|| Value::Bool(resolved.native_multimodal));
}

pub(crate) fn project_resolved_model_limits(
    metadata: &mut BTreeMap<String, Value>,
    context_length: Option<u64>,
    max_output_tokens: Option<u64>,
) {
    let has_positive_context = metadata
        .get("model_context_window")
        .and_then(Value::as_u64)
        .is_some_and(|value| value > 0);
    if !has_positive_context {
        if let Some(context_length) = context_length.filter(|value| *value > 0) {
            metadata.insert(
                "model_context_window".to_string(),
                Value::from(context_length),
            );
        }
    }
    if let Some(max_output_tokens) = max_output_tokens {
        metadata
            .entry("model_max_output_tokens".to_string())
            .or_insert_with(|| Value::from(max_output_tokens));
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("settings file not found: {0}")]
    MissingSettingsFile(String),
    #[error("failed to read settings file {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse settings file {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("invalid LLM settings: {0}")]
    InvalidSettings(String),
    #[error("unsupported chat backend: {0}")]
    UnsupportedBackend(String),
}

pub fn load_llm_settings_from_file(path: impl AsRef<Path>) -> Result<Value, ConfigError> {
    let path = path.as_ref();
    if !path.exists() {
        return Err(ConfigError::MissingSettingsFile(path.display().to_string()));
    }

    let content = fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.display().to_string(),
        source,
    })?;

    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    let parsed = match extension.as_deref() {
        Some("py") => settings_literal::parse_llm_settings_source(&content).map_err(|source| {
            ConfigError::Parse {
                path: path.display().to_string(),
                source: Box::new(source),
            }
        }),
        Some("json") => serde_json::from_str(&content).map_err(|source| ConfigError::Parse {
            path: path.display().to_string(),
            source: Box::new(source),
        }),
        Some("toml") => {
            let value: toml::Value =
                toml::from_str(&content).map_err(|source| ConfigError::Parse {
                    path: path.display().to_string(),
                    source: Box::new(source),
                })?;
            serde_json::to_value(value).map_err(|source| ConfigError::Parse {
                path: path.display().to_string(),
                source: Box::new(source),
            })
        }
        extension => {
            return Err(ConfigError::InvalidSettings(format!(
                "unsupported settings file extension: {}",
                extension.unwrap_or("<none>")
            )))
        }
    }?;
    canonical_settings_value(&parsed)
}

pub(crate) fn canonical_settings_value(settings: &Value) -> Result<Value, ConfigError> {
    let object = settings.as_object().ok_or_else(|| {
        ConfigError::InvalidSettings("LLM_SETTINGS must be an object".to_string())
    })?;
    if object.get("VERSION").and_then(Value::as_str) != Some("2") {
        return Err(ConfigError::InvalidSettings(
            "LLM_SETTINGS.VERSION must be '2'".to_string(),
        ));
    }
    if !object.get("backends").is_some_and(Value::is_object) {
        return Err(ConfigError::InvalidSettings(
            "LLM_SETTINGS.backends must be an object".to_string(),
        ));
    }
    if !object.get("endpoints").is_some_and(Value::is_array) {
        return Err(ConfigError::InvalidSettings(
            "LLM_SETTINGS.endpoints must be an array".to_string(),
        ));
    }
    Ok(settings.clone())
}
