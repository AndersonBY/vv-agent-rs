mod agents;
mod queries;
mod runs;
mod runtime;
mod sessions;
mod task;

use std::collections::BTreeMap;
use std::sync::Arc;

use super::types::{AgentDefinition, AgentSDKOptions};
pub use queries::{
    query, query_with_options_and_agent, query_with_options_and_agent_in_workspace,
    query_with_options_and_agent_in_workspace_with_require_completed,
    query_with_options_and_agent_request,
    query_with_options_and_agent_request_with_require_completed,
    query_with_options_and_agent_with_require_completed,
};
pub use runs::{
    run, run_with_options_and_agent, run_with_options_and_agent_in_workspace,
    run_with_options_and_agent_request,
};
pub use runtime::RunAgent;
pub use sessions::{
    create_agent_session, create_agent_session_with_id, create_agent_session_with_id_and_workspace,
    create_agent_session_with_id_and_workspace_and_shared_state,
    create_agent_session_with_shared_state, create_agent_session_with_workspace,
    create_agent_session_with_workspace_and_shared_state,
};

#[derive(Clone)]
#[deprecated(note = "Use vv_agent::Runner as the primary execution entrypoint for new SDK code.")]
pub struct AgentSDKClient {
    pub options: AgentSDKOptions,
    default_agent: Option<AgentDefinition>,
    agents: BTreeMap<String, AgentDefinition>,
    prompt_templates: BTreeMap<String, String>,
    resource_skill_directories: Vec<String>,
    resource_diagnostics: Vec<String>,
    runtime: Arc<dyn RunAgent + Send + Sync>,
}
