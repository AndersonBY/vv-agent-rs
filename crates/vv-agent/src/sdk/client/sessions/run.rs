use std::sync::Arc;

use crate::sdk::session::AgentSessionRunRequest;
use crate::sdk::types::{AgentDefinition, AgentRun};

use super::super::AgentSDKClient;

pub(super) fn session_run_executor(
    client: &AgentSDKClient,
    definition: &AgentDefinition,
) -> Arc<dyn Fn(AgentSessionRunRequest) -> Result<AgentRun, String> + Send + Sync> {
    let runtime = client.runtime.clone();
    let definition_for_run = definition.clone();
    let stream_callback = client.options.stream_callback.clone();

    Arc::new(move |mut request: AgentSessionRunRequest| {
        if request.stream_callback.is_none() {
            request.stream_callback = stream_callback.clone();
        }
        runtime.run_with_session(&definition_for_run, request)
    })
}
