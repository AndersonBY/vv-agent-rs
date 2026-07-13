use std::path::PathBuf;

use serde_json::json;
use thiserror::Error;

use crate::app_server::protocol::{AppModelInfo, ModelListParams, ModelListResponse};
use crate::types::Metadata;
use crate::{Agent, RunConfig};

#[derive(Debug, Clone, PartialEq)]
pub struct AgentResolutionRequest {
    pub thread_id: String,
    pub agent_key: String,
    pub cwd: Option<PathBuf>,
    pub metadata: Metadata,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunConfigResolutionRequest {
    pub thread_id: String,
    pub agent_key: String,
    pub cwd: Option<PathBuf>,
    pub metadata: Metadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct AppServerHostError {
    message: String,
}

impl AppServerHostError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl From<String> for AppServerHostError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

impl From<&str> for AppServerHostError {
    fn from(message: &str) -> Self {
        Self::new(message)
    }
}

pub trait AppServerHost: Send + Sync {
    fn resolve_agent(&self, request: &AgentResolutionRequest) -> Result<Agent, AppServerHostError>;

    fn build_run_config(
        &self,
        request: &RunConfigResolutionRequest,
    ) -> Result<RunConfig, AppServerHostError>;

    fn list_models(
        &self,
        request: &ModelListParams,
    ) -> Result<ModelListResponse, AppServerHostError>;
}

#[derive(Clone, Default)]
pub struct DefaultAppServerHost {
    agent: Option<Agent>,
    run_config: Option<RunConfig>,
    models: Vec<AppModelInfo>,
}

impl DefaultAppServerHost {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_agent(agent: Agent) -> Self {
        Self::new().with_agent(agent)
    }

    pub fn with_agent(mut self, agent: Agent) -> Self {
        self.agent = Some(agent);
        self
    }

    pub fn with_run_config(mut self, run_config: RunConfig) -> Self {
        self.run_config = Some(run_config);
        self
    }

    pub fn with_models(mut self, models: Vec<AppModelInfo>) -> Self {
        self.models = models;
        self
    }
}

impl AppServerHost for DefaultAppServerHost {
    fn resolve_agent(&self, request: &AgentResolutionRequest) -> Result<Agent, AppServerHostError> {
        if let Some(agent) = &self.agent {
            return Ok(agent.clone());
        }
        let name = if request.agent_key.trim().is_empty() {
            "assistant"
        } else {
            request.agent_key.as_str()
        };
        Agent::builder(name)
            .instructions("You are the default vv-agent App Server assistant.")
            .build()
            .map_err(AppServerHostError::new)
    }

    fn build_run_config(
        &self,
        request: &RunConfigResolutionRequest,
    ) -> Result<RunConfig, AppServerHostError> {
        if let Some(run_config) = &self.run_config {
            return Ok(run_config.clone());
        }
        let mut run_config = RunConfig {
            workspace: request.cwd.clone(),
            ..RunConfig::default()
        };
        run_config
            .metadata
            .insert("agent_key".to_string(), json!(request.agent_key));
        Ok(run_config)
    }

    fn list_models(
        &self,
        _request: &ModelListParams,
    ) -> Result<ModelListResponse, AppServerHostError> {
        Ok(ModelListResponse {
            models: self.models.clone(),
        })
    }
}
