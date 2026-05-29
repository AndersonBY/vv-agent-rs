use std::path::PathBuf;

use crate::sdk::session::AgentSession;
use crate::types::Metadata;

use super::super::AgentSDKClient;
use super::base::{
    create_agent_session, create_agent_session_with_id, create_agent_session_with_id_and_workspace,
    create_agent_session_with_id_and_workspace_and_shared_state,
    create_agent_session_with_shared_state, create_agent_session_with_workspace,
    create_agent_session_with_workspace_and_shared_state,
};

impl AgentSDKClient {
    pub fn create_default_session(&self) -> Result<AgentSession, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call create_session_with_agent(...) or register named agents first.",
            "Multiple agents configured. Call create_agent_session_by_name(name) with one of:",
        )?;
        Ok(create_agent_session(self, name, definition))
    }

    pub fn create_default_session_with_workspace(
        &self,
        workspace: impl Into<PathBuf>,
    ) -> Result<AgentSession, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call create_session_with_agent(...) or register named agents first.",
            "Multiple agents configured. Call create_agent_session_by_name_in_workspace(name, workspace) with one of:",
        )?;
        Ok(create_agent_session_with_workspace(
            self, name, definition, workspace,
        ))
    }

    pub fn create_default_session_with_id(
        &self,
        session_id: impl Into<String>,
    ) -> Result<AgentSession, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call create_session_with_agent(...) or register named agents first.",
            "Multiple agents configured. Call create_agent_session_by_name_with_id(name, session_id) with one of:",
        )?;
        Ok(create_agent_session_with_id(
            self, name, definition, session_id,
        ))
    }

    pub fn create_default_session_with_id_and_workspace(
        &self,
        session_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
    ) -> Result<AgentSession, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call create_session_with_agent(...) or register named agents first.",
            "Multiple agents configured. Call create_agent_session_by_name_with_id_and_workspace(name, session_id, workspace) with one of:",
        )?;
        Ok(create_agent_session_with_id_and_workspace(
            self, name, definition, session_id, workspace,
        ))
    }

    pub fn create_default_session_with_workspace_and_shared_state(
        &self,
        workspace: impl Into<PathBuf>,
        shared_state: Metadata,
    ) -> Result<AgentSession, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call create_session_with_agent(...) or register named agents first.",
            "Multiple agents configured. Call create_agent_session_by_name_in_workspace(name, workspace) with one of:",
        )?;
        Ok(create_agent_session_with_workspace_and_shared_state(
            self,
            name,
            definition,
            workspace,
            shared_state,
        ))
    }

    pub fn create_default_session_with_id_workspace_and_shared_state(
        &self,
        session_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
        shared_state: Metadata,
    ) -> Result<AgentSession, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call create_session_with_agent(...) or register named agents first.",
            "Multiple agents configured. Call create_agent_session_by_name_with_id_and_workspace(name, session_id, workspace) with one of:",
        )?;
        Ok(create_agent_session_with_id_and_workspace_and_shared_state(
            self,
            name,
            definition,
            session_id,
            workspace,
            shared_state,
        ))
    }

    pub fn create_default_session_with_shared_state(
        &self,
        shared_state: Metadata,
    ) -> Result<AgentSession, String> {
        let (name, definition) = self.default_or_only_agent(
            "No agent configured. Call create_session_with_agent(...) or register named agents first.",
            "Multiple agents configured. Call create_agent_session_by_name_with_shared_state(name, shared_state) with one of:",
        )?;
        Ok(create_agent_session_with_shared_state(
            self,
            name,
            definition,
            shared_state,
        ))
    }
}
