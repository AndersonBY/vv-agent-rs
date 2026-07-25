use std::collections::{BTreeMap, BTreeSet};

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::hash::sha256_hex;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PromptSection {
    pub id: String,
    pub text: String,
    pub stable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_hint: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptSectionWire {
    id: String,
    text: String,
    stable: bool,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    cache_hint: Option<String>,
    #[serde(default)]
    metadata: BTreeMap<String, Value>,
}

impl<'de> Deserialize<'de> for PromptSection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = PromptSectionWire::deserialize(deserializer)?;
        Self::try_new(wire.id, wire.text, wire.stable)
            .and_then(|section| {
                Ok(Self {
                    source: normalize_optional_strict(wire.source, "source")?,
                    cache_hint: normalize_optional_strict(wire.cache_hint, "cache_hint")?,
                    metadata: wire.metadata,
                    ..section
                })
            })
            .and_then(|section| section.validate().map(|()| section))
            .map_err(D::Error::custom)
    }
}

impl PromptSection {
    pub fn new(id: impl Into<String>, text: impl Into<String>, stable: bool) -> Self {
        Self {
            id: trim_ascii_whitespace(&id.into()).to_string(),
            text: trim_ascii_whitespace(&text.into()).to_string(),
            stable,
            source: None,
            cache_hint: None,
            metadata: BTreeMap::new(),
        }
    }

    pub fn try_new(
        id: impl Into<String>,
        text: impl Into<String>,
        stable: bool,
    ) -> Result<Self, String> {
        let section = Self::new(id, text, stable);
        section.validate()?;
        Ok(section)
    }

    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = normalize_optional(Some(source.into()));
        self
    }

    pub fn cache_hint(mut self, cache_hint: impl Into<String>) -> Self {
        self.cache_hint = normalize_optional(Some(cache_hint.into()));
        self
    }

    pub fn metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    pub fn validate(&self) -> Result<(), String> {
        if trim_ascii_whitespace(&self.id).is_empty() {
            return Err("prompt section id must be non-empty".to_string());
        }
        if trim_ascii_whitespace(&self.text).is_empty() {
            return Err("prompt section text must be non-empty".to_string());
        }
        for (name, value) in [
            ("source", self.source.as_deref()),
            ("cache_hint", self.cache_hint.as_deref()),
        ] {
            if value.is_some_and(|value| trim_ascii_whitespace(value).is_empty()) {
                return Err(format!(
                    "prompt section {name} must be omitted or non-empty"
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PromptBundle {
    pub sections: Vec<PromptSection>,
    pub stable_hash: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptBundleWire {
    sections: Vec<PromptSection>,
    stable_hash: String,
}

impl<'de> Deserialize<'de> for PromptBundle {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = PromptBundleWire::deserialize(deserializer)?;
        let bundle = Self {
            sections: wire.sections,
            stable_hash: wire.stable_hash,
        };
        bundle.validate().map_err(D::Error::custom)?;
        Ok(bundle)
    }
}

impl PromptBundle {
    pub fn new(sections: Vec<PromptSection>) -> Result<Self, String> {
        if sections.is_empty() {
            return Err("prompt bundle sections must be non-empty".to_string());
        }
        validate_sections(&sections)?;
        let stable_hash = stable_hash(&sections)?;
        Ok(Self {
            sections,
            stable_hash,
        })
    }

    pub fn from_instruction_text(text: impl Into<String>) -> Result<Self, String> {
        Self::new(vec![PromptSection::try_new(
            "agent_instructions",
            text,
            true,
        )?
        .source("agent.instructions")])
    }

    pub fn flatten(&self) -> String {
        self.sections
            .iter()
            .map(|section| section.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.sections.is_empty() {
            return Err("prompt bundle sections must be non-empty".to_string());
        }
        validate_sections(&self.sections)?;
        let expected = stable_hash(&self.sections)?;
        if self.stable_hash != expected {
            return Err("prompt bundle stable_hash does not match stable sections".to_string());
        }
        Ok(())
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("PromptBundle contains JSON values")
    }

    pub fn from_value(value: &Value) -> Result<Self, String> {
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())
    }
}

fn validate_sections(sections: &[PromptSection]) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for section in sections {
        section.validate()?;
        if !ids.insert(section.id.as_str()) {
            return Err(format!("duplicate prompt section id: {}", section.id));
        }
    }
    Ok(())
}

fn stable_hash(sections: &[PromptSection]) -> Result<String, String> {
    let stable = sections
        .iter()
        .filter(|section| section.stable)
        .collect::<Vec<_>>();
    let bytes = serde_json_canonicalizer::to_vec(&stable)
        .map_err(|error| format!("prompt stable sections cannot be canonicalized: {error}"))?;
    Ok(sha256_hex(&bytes))
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = trim_ascii_whitespace(&value).to_string();
        (!value.is_empty()).then_some(value)
    })
}

fn normalize_optional_strict(
    value: Option<String>,
    field_name: &str,
) -> Result<Option<String>, String> {
    match value {
        None => Ok(None),
        Some(value) => {
            let value = trim_ascii_whitespace(&value).to_string();
            if value.is_empty() {
                Err(format!(
                    "prompt section {field_name} must be omitted or non-empty"
                ))
            } else {
                Ok(Some(value))
            }
        }
    }
}

fn trim_ascii_whitespace(value: &str) -> &str {
    value.trim_matches(|character| matches!(character, '\u{0009}'..='\u{000D}' | '\u{0020}'))
}
