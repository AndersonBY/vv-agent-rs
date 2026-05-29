use std::collections::BTreeMap;

use crate::sdk::types::AgentDefinition;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiscoveredResources {
    pub agents: BTreeMap<String, AgentDefinition>,
    pub prompts: BTreeMap<String, String>,
    pub skill_directories: Vec<String>,
    pub diagnostics: Vec<String>,
}
