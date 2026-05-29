use std::path::PathBuf;

use crate::sdk::session::AgentSession;
use crate::sdk::types::AgentDefinition;
use crate::types::Metadata;

use super::super::AgentSDKClient;
use super::run::session_run_executor;

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
    AgentSession::new_with_context_and_shared_state(
        session_run_executor(client, &definition),
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
    AgentSession::new_with_context_and_session_id_and_shared_state(
        session_run_executor(client, &definition),
        session_id,
        agent_name,
        definition,
        workspace,
        shared_state,
    )
}

impl AgentSDKClient {
    pub fn create_session(
        &self,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
    ) -> AgentSession {
        create_agent_session(self, agent_name, definition)
    }

    pub fn create_session_with_shared_state(
        &self,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        shared_state: Metadata,
    ) -> AgentSession {
        create_agent_session_with_shared_state(self, agent_name, definition, shared_state)
    }

    pub fn create_session_with_id(
        &self,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        session_id: impl Into<String>,
    ) -> AgentSession {
        create_agent_session_with_id(self, agent_name, definition, session_id)
    }

    pub fn create_session_with_workspace(
        &self,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        workspace: impl Into<PathBuf>,
    ) -> AgentSession {
        create_agent_session_with_workspace(self, agent_name, definition, workspace)
    }

    pub fn create_session_with_workspace_and_shared_state(
        &self,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        workspace: impl Into<PathBuf>,
        shared_state: Metadata,
    ) -> AgentSession {
        create_agent_session_with_workspace_and_shared_state(
            self,
            agent_name,
            definition,
            workspace,
            shared_state,
        )
    }

    pub fn create_session_with_id_and_workspace(
        &self,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        session_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
    ) -> AgentSession {
        create_agent_session_with_id_and_workspace(
            self, agent_name, definition, session_id, workspace,
        )
    }

    pub fn create_session_with_id_workspace_and_shared_state(
        &self,
        agent_name: impl Into<String>,
        definition: AgentDefinition,
        session_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
        shared_state: Metadata,
    ) -> AgentSession {
        create_agent_session_with_id_and_workspace_and_shared_state(
            self,
            agent_name,
            definition,
            session_id,
            workspace,
            shared_state,
        )
    }
}
