use std::collections::BTreeMap;

use serde_json::Value;

use crate::skills::errors::SkillParseError;

use super::value::normalize_metadata_map;

pub fn parse_frontmatter(
    content: &str,
) -> Result<(BTreeMap<String, Value>, String), SkillParseError> {
    let Some(rest) = content.strip_prefix("---") else {
        return Err(SkillParseError::new(
            "SKILL.md must start with YAML frontmatter (---)",
        ));
    };
    let Some((frontmatter, body)) = rest.split_once("---") else {
        return Err(SkillParseError::new(
            "SKILL.md frontmatter not properly closed with ---",
        ));
    };

    let parsed = serde_yaml::from_str::<Value>(frontmatter)
        .map_err(|error| SkillParseError::new(format!("Invalid YAML in frontmatter: {error}")))?;
    let Value::Object(object) = parsed else {
        return Err(SkillParseError::new(
            "SKILL.md frontmatter must be a YAML mapping",
        ));
    };

    let mut metadata = BTreeMap::new();
    for (key, value) in object {
        if key == "metadata" {
            metadata.insert(key, normalize_metadata_map(value));
        } else {
            metadata.insert(key, value);
        }
    }
    Ok((metadata, body.trim().to_string()))
}
