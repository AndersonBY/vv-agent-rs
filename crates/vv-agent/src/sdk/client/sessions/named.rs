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
    pub fn create_agent_session_by_name(
        &self,
        agent_name: impl AsRef<str>,
    ) -> Result<AgentSession, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(create_agent_session(self, agent_name, definition))
    }

    pub fn create_agent_session_by_name_in_workspace(
        &self,
        agent_name: impl AsRef<str>,
        workspace: impl Into<PathBuf>,
    ) -> Result<AgentSession, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(create_agent_session_with_workspace(
            self, agent_name, definition, workspace,
        ))
    }

    pub fn create_agent_session_by_name_with_id(
        &self,
        agent_name: impl AsRef<str>,
        session_id: impl Into<String>,
    ) -> Result<AgentSession, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(create_agent_session_with_id(
            self, agent_name, definition, session_id,
        ))
    }

    pub fn create_agent_session_by_name_with_id_and_workspace(
        &self,
        agent_name: impl AsRef<str>,
        session_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
    ) -> Result<AgentSession, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(create_agent_session_with_id_and_workspace(
            self, agent_name, definition, session_id, workspace,
        ))
    }

    pub fn create_agent_session_by_name_in_workspace_with_shared_state(
        &self,
        agent_name: impl AsRef<str>,
        workspace: impl Into<PathBuf>,
        shared_state: Metadata,
    ) -> Result<AgentSession, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(create_agent_session_with_workspace_and_shared_state(
            self,
            agent_name,
            definition,
            workspace,
            shared_state,
        ))
    }

    pub fn create_agent_session_by_name_with_id_workspace_and_shared_state(
        &self,
        agent_name: impl AsRef<str>,
        session_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
        shared_state: Metadata,
    ) -> Result<AgentSession, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(create_agent_session_with_id_and_workspace_and_shared_state(
            self,
            agent_name,
            definition,
            session_id,
            workspace,
            shared_state,
        ))
    }

    pub fn create_agent_session_by_name_with_shared_state(
        &self,
        agent_name: impl AsRef<str>,
        shared_state: Metadata,
    ) -> Result<AgentSession, String> {
        let agent_name = agent_name.as_ref().trim();
        let definition = self.get_agent(agent_name)?.clone();
        Ok(create_agent_session_with_shared_state(
            self,
            agent_name,
            definition,
            shared_state,
        ))
    }
}
