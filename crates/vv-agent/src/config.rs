use std::fs;
use std::path::Path;

use serde_json::Value;
use thiserror::Error;

use crate::types::AgentTask;

mod api_keys;
mod model_resolution;
mod settings_literal;

pub use api_keys::decode_api_key;
pub use model_resolution::{
    build_vv_llm_from_local_settings, build_vv_llm_settings, resolve_model_endpoint,
};

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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemorySummaryDefaults {
    pub backend: Option<String>,
    pub model: Option<String>,
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
    if let Some(context_length) = resolved.context_length {
        task.metadata
            .entry("model_context_window".to_string())
            .or_insert_with(|| Value::from(context_length));
    }
    if let Some(max_output_tokens) = resolved.max_output_tokens {
        task.metadata
            .entry("model_max_output_tokens".to_string())
            .or_insert_with(|| Value::from(max_output_tokens));
    }
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

pub fn load_memory_summary_defaults_from_file(path: &Path) -> MemorySummaryDefaults {
    let Ok(source) = fs::read_to_string(path) else {
        return MemorySummaryDefaults::default();
    };
    MemorySummaryDefaults {
        backend: parse_string_setting(
            &source,
            &[
                "DEFAULT_USER_MEMORY_SUMMARIZE_BACKEND",
                "DEFAULT_MEMORY_SUMMARIZE_BACKEND",
                "VV_AGENT_MEMORY_SUMMARY_BACKEND",
                "memory_summary_backend",
                "compress_memory_summary_backend",
            ],
        ),
        model: parse_string_setting(
            &source,
            &[
                "DEFAULT_USER_MEMORY_SUMMARIZE_MODEL",
                "DEFAULT_MEMORY_SUMMARIZE_MODEL",
                "VV_AGENT_MEMORY_SUMMARY_MODEL",
                "memory_summary_model",
                "compress_memory_summary_model",
            ],
        ),
    }
}

fn parse_string_setting(source: &str, targets: &[&str]) -> Option<String> {
    parse_json_string_setting(source, targets)
        .or_else(|| settings_literal::parse_string_assignment(source, targets))
}

fn parse_json_string_setting(source: &str, targets: &[&str]) -> Option<String> {
    let value = serde_json::from_str::<Value>(source).ok()?;
    for target in targets {
        let Some(raw) = value.get(*target).and_then(Value::as_str) else {
            continue;
        };
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
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

    match path.extension().and_then(|ext| ext.to_str()) {
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
        _ => serde_json::from_str(&content).map_err(|source| ConfigError::Parse {
            path: path.display().to_string(),
            source: Box::new(source),
        }),
    }
}
