use std::path::PathBuf;
use std::sync::Arc;

use crate::types::Metadata;

use super::super::session::{AgentSession, AgentSessionRunRequest};
use super::super::types::AgentDefinition;
use super::AgentSDKClient;

pub fn create_agent_session(
    client: &AgentSDKClient,
    agent_name: impl Into<String>,
    definition: AgentDefinition,
) -> AgentSession {
    create_agent_session_with_workspace(
        client,
        agent_name,
        definition,
        client.options.workspace.clone(),
    )
}

pub fn create_agent_session_with_shared_state(
    client: &AgentSDKClient,
    agent_name: impl Into<String>,
    definition: AgentDefinition,
    shared_state: Metadata,
) -> AgentSession {
    create_agent_session_with_workspace_and_shared_state(
        client,
        agent_name,
        definition,
        client.options.workspace.clone(),
        shared_state,
    )
}

pub fn create_agent_session_with_workspace(
    client: &AgentSDKClient,
    agent_name: impl Into<String>,
    definition: AgentDefinition,
    workspace: impl Into<PathBuf>,
) -> AgentSession {
    create_agent_session_with_workspace_and_shared_state(
        client,
        agent_name,
        definition,
        workspace,
        Metadata::new(),
    )
}

pub fn create_agent_session_with_workspace_and_shared_state(
    client: &AgentSDKClient,
    agent_name: impl Into<String>,
    definition: AgentDefinition,
    workspace: impl Into<PathBuf>,
    shared_state: Metadata,
) -> AgentSession {
    let definition = client.effective_definition(definition);
    let runtime = client.runtime.clone();
    let definition_for_run = definition.clone();
    let stream_callback = client.options.stream_callback.clone();
    let execute_run = Arc::new(move |mut request: AgentSessionRunRequest| {
        if request.stream_callback.is_none() {
            request.stream_callback = stream_callback.clone();
        }
        runtime.run_with_session(&definition_for_run, request)
    });
    AgentSession::new_with_context_and_shared_state(
        execute_run,
        agent_name,
        definition,
        workspace,
        shared_state,
    )
}

pub fn create_agent_session_with_id(
    client: &AgentSDKClient,
    agent_name: impl Into<String>,
    definition: AgentDefinition,
    session_id: impl Into<String>,
) -> AgentSession {
    create_agent_session_with_id_and_workspace(
        client,
        agent_name,
        definition,
        session_id,
        client.options.workspace.clone(),
    )
}

pub fn create_agent_session_with_id_and_workspace(
    client: &AgentSDKClient,
    agent_name: impl Into<String>,
    definition: AgentDefinition,
    session_id: impl Into<String>,
    workspace: impl Into<PathBuf>,
) -> AgentSession {
    create_agent_session_with_id_and_workspace_and_shared_state(
        client,
        agent_name,
        definition,
        session_id,
        workspace,
        Metadata::new(),
    )
}

pub fn create_agent_session_with_id_and_workspace_and_shared_state(
    client: &AgentSDKClient,
    agent_name: impl Into<String>,
    definition: AgentDefinition,
    session_id: impl Into<String>,
    workspace: impl Into<PathBuf>,
    shared_state: Metadata,
) -> AgentSession {
    let definition = client.effective_definition(definition);
    let runtime = client.runtime.clone();
    let definition_for_run = definition.clone();
    let stream_callback = client.options.stream_callback.clone();
    let execute_run = Arc::new(move |mut request: AgentSessionRunRequest| {
        if request.stream_callback.is_none() {
            request.stream_callback = stream_callback.clone();
        }
        runtime.run_with_session(&definition_for_run, request)
    });
    AgentSession::new_with_context_and_session_id_and_shared_state(
        execute_run,
        session_id,
        agent_name,
        definition,
        workspace,
        shared_state,
    )
}
