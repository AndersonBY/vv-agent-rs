use std::fs;
use std::path::Path;

use serde_json::Value;
use thiserror::Error;

use crate::llm::VvLlmClient;

const MODEL_ALIAS_MAP: &[(&str, &str)] = &[("kimi-k2.5", "kimi-k2-thinking")];

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
            endpoint_options,
        }
    }

    pub fn endpoint(&self) -> Option<&EndpointConfig> {
        self.endpoint_options.first().map(|option| &option.endpoint)
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

    match path.extension().and_then(|ext| ext.to_str()) {
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

pub fn resolve_model_endpoint(
    settings: &Value,
    backend: &str,
    model: &str,
) -> Result<ResolvedModelConfig, ConfigError> {
    let settings = normalize_llm_settings(settings)?;
    let backend_type = backend_type_from_str(backend)?;
    let selected_model = select_model_alias(&settings, backend, model);
    let resolved = settings
        .resolve_chat_model(backend_type, &selected_model)
        .map_err(|error| ConfigError::InvalidSettings(error.to_string()))?;
    Ok(resolved_from_vv_llm(
        backend,
        model,
        &selected_model,
        resolved,
    ))
}

pub fn build_openai_llm_from_local_settings(
    settings_path: impl AsRef<Path>,
    backend: &str,
    model: &str,
    timeout_seconds: f64,
) -> Result<(VvLlmClient, ResolvedModelConfig), ConfigError> {
    let settings_value = load_llm_settings_from_file(settings_path)?;
    let settings = normalize_llm_settings(&settings_value)?;
    let backend_type = backend_type_from_str(backend)?;
    let selected_model = select_model_alias(&settings, backend, model);
    let vv_resolved = settings
        .resolve_chat_model(backend_type, &selected_model)
        .map_err(|error| ConfigError::InvalidSettings(error.to_string()))?;
    let resolved = resolved_from_vv_llm(backend, model, &selected_model, vv_resolved.clone());
    let chat_client = vv_llm::create_chat_client_from_resolved(vv_resolved)
        .map_err(|error| ConfigError::InvalidSettings(error.to_string()))?;
    let llm = VvLlmClient::new(
        backend,
        resolved.selected_model.clone(),
        resolved.model_id.clone(),
        chat_client,
        timeout_seconds,
    );
    Ok((llm, resolved))
}

fn normalize_llm_settings(settings: &Value) -> Result<vv_llm::LlmSettings, ConfigError> {
    let settings = settings
        .as_object()
        .and_then(|settings_object| {
            settings_object
                .get("LLM_SETTINGS")
                .filter(|embedded| embedded.get("endpoints").is_some())
                .or(Some(settings))
        })
        .ok_or_else(|| ConfigError::InvalidSettings("settings must be an object".to_string()))?;
    serde_json::from_value(settings.clone()).map_err(|error| ConfigError::Parse {
        path: "LLM_SETTINGS".to_string(),
        source: Box::new(error),
    })
}

fn backend_type_from_str(backend: &str) -> Result<vv_llm::BackendType, ConfigError> {
    match backend {
        "openai" => Ok(vv_llm::BackendType::OpenAI),
        "zhipuai" => Ok(vv_llm::BackendType::ZhiPuAI),
        "minimax" => Ok(vv_llm::BackendType::MiniMax),
        "moonshot" => Ok(vv_llm::BackendType::Moonshot),
        "anthropic" => Ok(vv_llm::BackendType::Anthropic),
        "mistral" => Ok(vv_llm::BackendType::Mistral),
        "deepseek" => Ok(vv_llm::BackendType::DeepSeek),
        "qwen" => Ok(vv_llm::BackendType::Qwen),
        "groq" => Ok(vv_llm::BackendType::Groq),
        "local" => Ok(vv_llm::BackendType::Local),
        "yi" => Ok(vv_llm::BackendType::Yi),
        "gemini" => Ok(vv_llm::BackendType::Gemini),
        "baichuan" => Ok(vv_llm::BackendType::Baichuan),
        "stepfun" => Ok(vv_llm::BackendType::StepFun),
        "xai" => Ok(vv_llm::BackendType::XAI),
        "xiaomi" => Ok(vv_llm::BackendType::Xiaomi),
        "ernie" => Ok(vv_llm::BackendType::Ernie),
        other => Err(ConfigError::UnsupportedBackend(other.to_string())),
    }
}

fn select_model_alias(settings: &vv_llm::LlmSettings, backend: &str, model: &str) -> String {
    if settings
        .backends
        .get(backend)
        .is_some_and(|config| config.models.contains_key(model))
    {
        return model.to_string();
    }
    MODEL_ALIAS_MAP
        .iter()
        .find(|(alias, _)| *alias == model)
        .map(|(_, target)| target.to_string())
        .unwrap_or_else(|| model.to_string())
}

fn resolved_from_vv_llm(
    backend: &str,
    requested_model: &str,
    selected_model: &str,
    resolved: vv_llm::ResolvedModelConfig,
) -> ResolvedModelConfig {
    ResolvedModelConfig::new(
        backend,
        requested_model,
        selected_model,
        resolved.model_id.clone(),
        vec![EndpointOption {
            endpoint: EndpointConfig {
                endpoint_id: resolved.endpoint.id,
                api_key: resolved.endpoint.api_key.unwrap_or_default(),
                api_base: resolved.endpoint.api_base.unwrap_or_default(),
                endpoint_type: resolved
                    .endpoint
                    .endpoint_type
                    .unwrap_or_else(|| "default".to_string()),
            },
            model_id: resolved.model_id,
        }],
    )
}
