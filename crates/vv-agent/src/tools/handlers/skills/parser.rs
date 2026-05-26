use std::path::{Path, PathBuf};

use super::models::{ParsedFrontmatter, SkillEntry};

pub(super) fn entries_from_path(
    raw_path: &str,
    workspace: &Path,
    load_instructions: bool,
) -> Vec<SkillEntry> {
    if raw_path.is_empty() || !path_exists(raw_path, workspace) {
        return Vec::new();
    }
    let resolved = resolve_skill_path(raw_path, workspace);
    if resolved.is_dir() && find_skill_md(&resolved).is_none() {
        let mut entries = Vec::new();
        for skill_dir in discover_skill_dirs(&resolved) {
            entries.push(load_entry(&skill_dir, workspace, load_instructions));
        }
        return entries;
    }
    vec![load_entry(&resolved, workspace, load_instructions)]
}

pub(super) fn path_exists(raw_path: &str, workspace: &Path) -> bool {
    let path = Path::new(raw_path);
    if path.exists() {
        return true;
    }
    if !path.is_absolute() {
        return workspace.join(path).exists();
    }
    false
}

fn resolve_skill_path(raw_path: &str, workspace: &Path) -> PathBuf {
    let path = PathBuf::from(raw_path);
    let path = if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    };
    if path.is_file()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("skill.md"))
    {
        return path.parent().map(Path::to_path_buf).unwrap_or(path);
    }
    path
}

fn discover_skill_dirs(root: &Path) -> Vec<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    let mut discovered = Vec::new();
    while let Some(path) = stack.pop() {
        if !path.is_dir() {
            continue;
        }
        if find_skill_md(&path).is_some() {
            discovered.push(path);
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(&path) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    stack.push(entry.path());
                }
            }
        }
    }
    discovered.sort();
    discovered
}

fn find_skill_md(skill_dir: &Path) -> Option<PathBuf> {
    for name in ["SKILL.md", "skill.md"] {
        let candidate = skill_dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn load_entry(skill_dir: &Path, workspace: &Path, load_instructions: bool) -> SkillEntry {
    match read_skill_file(skill_dir, workspace, load_instructions) {
        Ok(entry) => entry,
        Err(error) => SkillEntry {
            name: skill_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("skill")
                .to_string(),
            description: String::new(),
            location: Some(skill_dir.display().to_string()),
            load_error: Some(error),
            ..SkillEntry::default()
        },
    }
}

fn read_skill_file(
    skill_dir: &Path,
    workspace: &Path,
    load_instructions: bool,
) -> Result<SkillEntry, String> {
    let skill_md = find_skill_md(skill_dir)
        .ok_or_else(|| format!("SKILL.md not found in {}", skill_dir.display()))?;
    let content = std::fs::read_to_string(&skill_md).map_err(|error| error.to_string())?;
    let (frontmatter, body) = parse_frontmatter(&content)?;
    let name = frontmatter
        .get("name")
        .cloned()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Missing required field in frontmatter: name".to_string())?;
    let description = frontmatter
        .get("description")
        .cloned()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Missing required field in frontmatter: description".to_string())?;
    Ok(SkillEntry {
        name,
        description,
        location: Some(relative_location(&skill_md, workspace)),
        instructions: load_instructions
            .then_some(body)
            .filter(|value| !value.is_empty()),
        compatibility: frontmatter.get("compatibility").cloned(),
        allowed_tools: frontmatter
            .get("allowed-tools")
            .or_else(|| frontmatter.get("allowed_tools"))
            .cloned(),
        metadata: frontmatter.metadata,
        load_error: None,
    })
}

fn parse_frontmatter(content: &str) -> Result<(ParsedFrontmatter, String), String> {
    let Some(rest) = content.strip_prefix("---") else {
        return Err("SKILL.md must start with YAML frontmatter (---)".to_string());
    };
    let Some((frontmatter, body)) = rest.split_once("\n---") else {
        return Err("SKILL.md frontmatter not properly closed with ---".to_string());
    };
    let mut parsed = ParsedFrontmatter::default();
    let mut in_metadata = false;
    for line in frontmatter.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() || trimmed.trim_start().starts_with('#') {
            continue;
        }
        if trimmed == "metadata:" {
            in_metadata = true;
            continue;
        }
        if in_metadata && (trimmed.starts_with(' ') || trimmed.starts_with('\t')) {
            if let Some((key, value)) = trimmed.trim().split_once(':') {
                parsed
                    .metadata
                    .insert(key.trim().to_string(), clean_yaml_scalar(value));
            }
            continue;
        }
        in_metadata = false;
        if let Some((key, value)) = trimmed.split_once(':') {
            parsed
                .scalars
                .insert(key.trim().to_string(), clean_yaml_scalar(value));
        }
    }
    Ok((parsed, body.trim().to_string()))
}

fn clean_yaml_scalar(value: &str) -> String {
    let value = value.trim();
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(value)
        .trim()
        .to_string()
}

fn relative_location(skill_md: &Path, workspace: &Path) -> String {
    skill_md
        .strip_prefix(workspace)
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|_| skill_md.to_string_lossy().to_string())
}
