use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{json, Value};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillProperties {
    pub name: String,
    pub description: String,
    pub license: Option<String>,
    pub compatibility: Option<String>,
    pub allowed_tools: Option<String>,
    pub metadata: BTreeMap<String, String>,
}

impl SkillProperties {
    pub fn to_value(&self) -> Value {
        let mut payload = serde_json::Map::from_iter([
            ("name".to_string(), Value::String(self.name.clone())),
            (
                "description".to_string(),
                Value::String(self.description.clone()),
            ),
        ]);
        if let Some(license) = &self.license {
            payload.insert("license".to_string(), Value::String(license.clone()));
        }
        if let Some(compatibility) = &self.compatibility {
            payload.insert(
                "compatibility".to_string(),
                Value::String(compatibility.clone()),
            );
        }
        if let Some(allowed_tools) = &self.allowed_tools {
            payload.insert(
                "allowed-tools".to_string(),
                Value::String(allowed_tools.clone()),
            );
        }
        if !self.metadata.is_empty() {
            payload.insert("metadata".to_string(), json!(self.metadata));
        }
        Value::Object(payload)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadedSkill {
    pub properties: SkillProperties,
    pub skill_md_path: PathBuf,
    pub instructions: String,
    pub warnings: Vec<String>,
}

impl LoadedSkill {
    pub fn name(&self) -> &str {
        &self.properties.name
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
    pub location: Option<String>,
    pub instructions: Option<String>,
    pub compatibility: Option<String>,
    pub allowed_tools: Option<String>,
    pub metadata: BTreeMap<String, String>,
    pub load_error: Option<String>,
}
