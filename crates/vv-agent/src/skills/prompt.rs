use std::path::{Path, PathBuf};

use serde_json::Value;

use super::errors::SkillError;
use super::models::SkillEntry;
use super::normalize::normalize_skill_list;
use super::parser::{find_skill_md, read_properties};

pub const MAX_SKILLS_PROMPT_CHARS: usize = 8000;

pub fn skill_entry_to_xml(entry: &SkillEntry, include_location: bool) -> String {
    let mut lines = vec![
        "<skill>".to_string(),
        "<name>".to_string(),
        escape_xml(&entry.name),
        "</name>".to_string(),
        "<description>".to_string(),
        escape_xml(&entry.description),
        "</description>".to_string(),
    ];
    if include_location {
        if let Some(location) = &entry.location {
            lines.extend([
                "<location>".to_string(),
                escape_xml(location),
                "</location>".to_string(),
            ]);
        }
    }
    lines.push("</skill>".to_string());
    lines.join("\n")
}

pub fn render_skills_xml(entries: &[SkillEntry], budget: usize) -> String {
    if entries.is_empty() {
        return "<available_skills>\n</available_skills>".to_string();
    }

    let full = render_all(entries, true);
    if full.chars().count() <= budget {
        return full;
    }

    let compact = render_all(entries, false);
    if compact.chars().count() <= budget {
        return compact;
    }

    let wrapper_overhead = "<available_skills>\n</available_skills>".chars().count() + 80;
    let mut remaining = budget.saturating_sub(wrapper_overhead);
    let mut lines = vec!["<available_skills>".to_string()];
    let mut included = 0usize;
    for entry in entries {
        let xml = skill_entry_to_xml(entry, false);
        let xml_chars = xml.chars().count();
        if xml_chars + 1 > remaining {
            break;
        }
        remaining = remaining.saturating_sub(xml_chars + 1);
        lines.push(xml);
        included += 1;
    }
    let omitted = entries.len().saturating_sub(included);
    if omitted > 0 {
        lines.push(format!(
            "<!-- {omitted} more skills available; use activate_skill to discover -->"
        ));
    }
    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

pub fn to_available_skills_xml(skill_dirs: &[PathBuf]) -> Result<String, SkillError> {
    if skill_dirs.is_empty() {
        return Ok("<available_skills>\n</available_skills>".to_string());
    }

    let mut lines = vec!["<available_skills>".to_string()];
    for skill_dir in skill_dirs {
        let normalized_dir = skill_dir
            .canonicalize()
            .unwrap_or_else(|_| skill_dir.clone());
        let properties = read_properties(&normalized_dir)?;
        let skill_md = find_skill_md(&normalized_dir);
        let entry = SkillEntry {
            name: properties.name,
            description: properties.description,
            location: skill_md.map(|path| path.to_string_lossy().replace('\\', "/")),
            compatibility: properties.compatibility,
            allowed_tools: properties.allowed_tools,
            metadata: properties.metadata,
            ..SkillEntry::default()
        };
        lines.push(skill_entry_to_xml(&entry, true));
    }
    lines.push("</available_skills>".to_string());
    Ok(lines.join("\n"))
}

pub fn metadata_to_prompt_entries(
    available_skills: Option<&Value>,
    workspace: Option<&Path>,
) -> Vec<SkillEntry> {
    normalize_skill_list(available_skills, workspace, false)
}

fn render_all(entries: &[SkillEntry], include_location: bool) -> String {
    let mut lines = vec!["<available_skills>".to_string()];
    for entry in entries {
        lines.push(skill_entry_to_xml(entry, include_location));
    }
    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}
