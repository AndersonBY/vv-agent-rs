use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;

use crate::config::{
    build_vv_llm_from_local_settings, resolve_model_endpoint, ResolvedModelConfig,
};
use crate::llm::{LLMClient, LlmClient, LlmRequest, ScriptStep, ScriptedLlmClient, VvLlmClient};
use crate::model_settings::ModelSettings;
use crate::types::LLMResponse;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelRef {
    Named(String),
    BackendModel { backend: String, model: String },
    Resolved(ResolvedModelConfig),
}

impl ModelRef {
    pub fn named(model: impl Into<String>) -> Self {
        Self::Named(model.into())
    }

    pub fn backend(backend: impl Into<String>, model: impl Into<String>) -> Self {
        Self::BackendModel {
            backend: backend.into(),
            model: model.into(),
        }
    }

    pub fn resolved(resolved: ResolvedModelConfig) -> Self {
        Self::Resolved(resolved)
    }

    pub fn model(&self) -> &str {
        match self {
            Self::Named(model) => model,
            Self::BackendModel { model, .. } => model,
            Self::Resolved(resolved) => resolved.selected_model.as_str(),
        }
    }

    pub fn backend_name(&self) -> Option<&str> {
        match self {
            Self::Named(_) => None,
            Self::BackendModel { backend, .. } => Some(backend),
            Self::Resolved(resolved) => Some(resolved.backend.as_str()),
        }
    }
}

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("model provider has no default backend for named model `{0}`")]
    MissingDefaultBackend(String),
    #[error("model backend mismatch: requested `{requested}`, provider `{provider}`")]
    BackendMismatch { requested: String, provider: String },
    #[error("{0}")]
    Config(String),
}

pub trait ModelProvider: Send + Sync {
    fn resolve(&self, model: &ModelRef) -> Result<ResolvedModelConfig, ModelError>;
    fn client(&self, resolved: &ResolvedModelConfig) -> Result<Arc<dyn LlmClient>, ModelError>;

    fn default_settings(&self, _resolved: &ResolvedModelConfig) -> ModelSettings {
        ModelSettings::default()
    }
}

#[derive(Clone)]
pub struct ScriptedModelProvider {
    backend: String,
    _default_model: String,
    llm: ScriptedLlmClient,
    context_length: Option<u64>,
    max_output_tokens: Option<u64>,
    default_settings: ModelSettings,
}

impl ScriptedModelProvider {
    pub fn new(
        backend: impl Into<String>,
        default_model: impl Into<String>,
        responses: Vec<LLMResponse>,
    ) -> Self {
        Self::from_steps(
            backend,
            default_model,
            responses.into_iter().map(ScriptStep::response).collect(),
        )
    }

    pub fn from_steps(
        backend: impl Into<String>,
        default_model: impl Into<String>,
        steps: Vec<ScriptStep>,
    ) -> Self {
        Self {
            backend: backend.into(),
            _default_model: default_model.into(),
            llm: ScriptedLlmClient::from_steps(steps),
            context_length: Some(128_000),
            max_output_tokens: Some(16_384),
            default_settings: ModelSettings::default(),
        }
    }

    pub fn from_callback(
        backend: impl Into<String>,
        default_model: impl Into<String>,
        callback: impl Fn(&LlmRequest) -> Result<LLMResponse, crate::llm::LlmError>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self::from_steps(backend, default_model, vec![ScriptStep::callback(callback)])
    }

    pub fn with_default_settings(mut self, settings: ModelSettings) -> Self {
        self.default_settings = settings;
        self
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
}

impl ModelProvider for ScriptedModelProvider {
    fn resolve(&self, model: &ModelRef) -> Result<ResolvedModelConfig, ModelError> {
        match model {
            ModelRef::Named(model) => Ok(ResolvedModelConfig::new(
                self.backend.clone(),
                model.clone(),
                model.clone(),
                model.clone(),
                Vec::new(),
            )
            .with_token_limits(self.context_length, self.max_output_tokens)),
            ModelRef::BackendModel { backend, model } => {
                if backend != &self.backend {
                    return Err(ModelError::BackendMismatch {
                        requested: backend.clone(),
                        provider: self.backend.clone(),
                    });
                }
                Ok(ResolvedModelConfig::new(
                    backend.clone(),
                    model.clone(),
                    model.clone(),
                    model.clone(),
                    Vec::new(),
                )
                .with_token_limits(self.context_length, self.max_output_tokens))
            }
            ModelRef::Resolved(resolved) => Ok(resolved.clone()),
        }
    }

    fn client(&self, _resolved: &ResolvedModelConfig) -> Result<Arc<dyn LlmClient>, ModelError> {
        Ok(Arc::new(self.llm.clone()))
    }

    fn default_settings(&self, _resolved: &ResolvedModelConfig) -> ModelSettings {
        self.default_settings.clone()
    }
}

impl Default for ScriptedModelProvider {
    fn default() -> Self {
        Self::new("scripted", "demo-model", Vec::new())
    }
}

#[derive(Debug, Clone)]
pub struct VvLlmModelProvider {
    settings_file: PathBuf,
    default_backend: Option<String>,
    timeout_seconds: f64,
}

impl VvLlmModelProvider {
    pub fn from_settings_file(path: impl Into<PathBuf>) -> Self {
        Self {
            settings_file: path.into(),
            default_backend: None,
            timeout_seconds: 90.0,
        }
    }

    pub fn with_default_backend(mut self, backend: impl Into<String>) -> Self {
        self.default_backend = Some(backend.into());
        self
    }

    pub fn with_timeout_seconds(mut self, timeout_seconds: f64) -> Self {
        self.timeout_seconds = timeout_seconds.max(1.0);
        self
    }

    pub fn settings_file(&self) -> &Path {
        &self.settings_file
    }
}

impl ModelProvider for VvLlmModelProvider {
    fn resolve(&self, model: &ModelRef) -> Result<ResolvedModelConfig, ModelError> {
        match model {
            ModelRef::Named(model) => {
                let Some(backend) = self.default_backend.as_deref() else {
                    return Err(ModelError::MissingDefaultBackend(model.clone()));
                };
                let settings = crate::config::load_llm_settings_from_file(&self.settings_file)
                    .map_err(|error| ModelError::Config(error.to_string()))?;
                resolve_model_endpoint(&settings, backend, model)
                    .map_err(|error| ModelError::Config(error.to_string()))
            }
            ModelRef::BackendModel { backend, model } => {
                let settings = crate::config::load_llm_settings_from_file(&self.settings_file)
                    .map_err(|error| ModelError::Config(error.to_string()))?;
                resolve_model_endpoint(&settings, backend, model)
                    .map_err(|error| ModelError::Config(error.to_string()))
            }
            ModelRef::Resolved(resolved) => Ok(resolved.clone()),
        }
    }

    fn client(&self, resolved: &ResolvedModelConfig) -> Result<Arc<dyn LlmClient>, ModelError> {
        let (llm, _) = build_vv_llm_from_local_settings(
            &self.settings_file,
            &resolved.backend,
            &resolved.selected_model,
            self.timeout_seconds,
        )
        .map_err(|error| ModelError::Config(error.to_string()))?;
        let llm: VvLlmClient = llm;
        Ok(Arc::new(llm) as Arc<dyn LLMClient>)
    }
}
