use std::path::PathBuf;
use std::sync::Arc;

use crate::model::{ModelProvider, VvLlmModelProvider};
use crate::run_config::RunConfig;
use crate::tools::ToolRegistry;

use super::Runner;

#[derive(Default)]
pub struct RunnerBuilder {
    model_provider: Option<Arc<dyn ModelProvider>>,
    settings_file: Option<PathBuf>,
    default_backend: Option<String>,
    workspace: Option<PathBuf>,
    tool_registry: Option<ToolRegistry>,
    default_run_config: RunConfig,
}

impl RunnerBuilder {
    pub fn model_provider(mut self, provider: impl ModelProvider + 'static) -> Self {
        self.model_provider = Some(Arc::new(provider));
        self
    }

    pub fn model_provider_arc(mut self, provider: Arc<dyn ModelProvider>) -> Self {
        self.model_provider = Some(provider);
        self
    }

    pub fn settings_file(mut self, settings_file: impl Into<PathBuf>) -> Self {
        self.settings_file = Some(settings_file.into());
        self
    }

    pub fn default_backend(mut self, default_backend: impl Into<String>) -> Self {
        self.default_backend = Some(default_backend.into());
        self
    }

    pub fn workspace(mut self, workspace: impl Into<PathBuf>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    pub fn tool_registry(mut self, registry: ToolRegistry) -> Self {
        self.tool_registry = Some(registry);
        self
    }

    pub fn default_run_config(mut self, config: RunConfig) -> Self {
        self.default_run_config = config;
        self
    }

    pub fn build(self) -> Result<Runner, String> {
        let model_provider = if let Some(provider) = self.model_provider {
            provider
        } else {
            let settings_file = self
                .settings_file
                .unwrap_or_else(|| PathBuf::from("local_settings.json"));
            let mut provider = VvLlmModelProvider::from_settings_file(settings_file);
            if let Some(default_backend) = self.default_backend {
                provider = provider.with_default_backend(default_backend);
            }
            Arc::new(provider)
        };
        Ok(Runner {
            model_provider,
            workspace: self
                .workspace
                .unwrap_or_else(|| PathBuf::from("./workspace")),
            tool_registry: self
                .tool_registry
                .unwrap_or_else(crate::tools::build_default_registry),
            default_run_config: self.default_run_config,
        })
    }
}
