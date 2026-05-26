use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct SkillEntry {
    pub(super) name: String,
    pub(super) description: String,
    pub(super) location: Option<String>,
    pub(super) instructions: Option<String>,
    pub(super) compatibility: Option<String>,
    pub(super) allowed_tools: Option<String>,
    pub(super) metadata: BTreeMap<String, String>,
    pub(super) load_error: Option<String>,
}

#[derive(Default)]
pub(super) struct ParsedFrontmatter {
    pub(super) scalars: BTreeMap<String, String>,
    pub(super) metadata: BTreeMap<String, String>,
}

impl ParsedFrontmatter {
    pub(super) fn get(&self, key: &str) -> Option<&String> {
        self.scalars.get(key)
    }
}
