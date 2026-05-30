use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::ResolvedModelConfig;
use crate::llm::LlmClient;
use crate::runtime::backends::RuntimeExecutionBackend;
use crate::runtime::{RuntimeEventHandler, RuntimeHook, StreamCallback};
use crate::sdk::resources::AgentResourceLoader;
use crate::tools::ToolRegistry;

pub type SdkLlmClient = Arc<dyn LlmClient>;
pub type LlmBuilder = Arc<
    dyn Fn(&Path, &str, &str, f64) -> Result<(SdkLlmClient, ResolvedModelConfig), String>
        + Send
        + Sync
        + 'static,
>;
pub use crate::runtime::RuntimeEventHandler as RuntimeLogHandler;
pub use LlmBuilder as LLMBuilder;
pub type ToolRegistryFactory = Arc<dyn Fn() -> ToolRegistry + Send + Sync + 'static>;

#[derive(Clone)]
pub struct AgentSDKOptions {
    pub settings_file: PathBuf,
    pub default_backend: String,
    pub workspace: PathBuf,
    pub timeout_seconds: f64,
    pub log_preview_chars: Option<usize>,
    pub llm_builder: Option<LlmBuilder>,
    pub tool_registry_factory: Option<ToolRegistryFactory>,
    pub log_handler: Option<RuntimeEventHandler>,
    pub execution_backend: Option<RuntimeExecutionBackend>,
    pub resource_loader: Option<AgentResourceLoader>,
    pub auto_discover_resources: bool,
    pub debug_dump_dir: Option<String>,
    pub stream_callback: Option<StreamCallback>,
    pub runtime_hooks: Vec<Arc<dyn RuntimeHook>>,
    pub bash_shell: Option<String>,
    pub windows_shell_priority: Vec<String>,
    pub bash_env: BTreeMap<String, String>,
}

impl std::fmt::Debug for AgentSDKOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentSDKOptions")
            .field("settings_file", &self.settings_file)
            .field("default_backend", &self.default_backend)
            .field("workspace", &self.workspace)
            .field("timeout_seconds", &self.timeout_seconds)
            .field("log_preview_chars", &self.log_preview_chars)
            .field("has_llm_builder", &self.llm_builder.is_some())
            .field("has_log_handler", &self.log_handler.is_some())
            .field(
                "has_tool_registry_factory",
                &self.tool_registry_factory.is_some(),
            )
            .field("execution_backend", &self.execution_backend)
            .field("has_resource_loader", &self.resource_loader.is_some())
            .field("auto_discover_resources", &self.auto_discover_resources)
            .field("debug_dump_dir", &self.debug_dump_dir)
            .field("has_stream_callback", &self.stream_callback.is_some())
            .field("runtime_hooks", &self.runtime_hooks.len())
            .field("bash_shell", &self.bash_shell)
            .field("windows_shell_priority", &self.windows_shell_priority)
            .field("bash_env", &self.bash_env)
            .finish()
    }
}

impl Default for AgentSDKOptions {
    fn default() -> Self {
        Self {
            settings_file: PathBuf::from("local_settings.json"),
            default_backend: "moonshot".to_string(),
            workspace: PathBuf::from("./workspace"),
            timeout_seconds: 90.0,
            log_preview_chars: None,
            llm_builder: None,
            tool_registry_factory: None,
            log_handler: None,
            execution_backend: None,
            resource_loader: None,
            auto_discover_resources: true,
            debug_dump_dir: None,
            stream_callback: None,
            runtime_hooks: Vec::new(),
            bash_shell: None,
            windows_shell_priority: Vec::new(),
            bash_env: BTreeMap::new(),
        }
    }
}
