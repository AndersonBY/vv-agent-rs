use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub fn find_skill_md(skill_dir: impl AsRef<Path>) -> Option<PathBuf> {
    let skill_dir = skill_dir.as_ref();
    for name in ["SKILL.md", "skill.md"] {
        let candidate = skill_dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub fn discover_skill_dirs(root: impl AsRef<Path>) -> Vec<PathBuf> {
    let root = root.as_ref();
    if !root.is_dir() {
        return Vec::new();
    }

    let mut discovered = Vec::new();
    let mut seen = BTreeSet::new();
    add_if_skill(root, &mut seen, &mut discovered);
    for candidate in recursive_dirs(root) {
        add_if_skill(&candidate, &mut seen, &mut discovered);
    }
    discovered
}

fn add_if_skill(dir_path: &Path, seen: &mut BTreeSet<PathBuf>, discovered: &mut Vec<PathBuf>) {
    let normalized = dir_path
        .canonicalize()
        .unwrap_or_else(|_| dir_path.to_path_buf());
    if seen.contains(&normalized) || find_skill_md(&normalized).is_none() {
        return;
    }
    seen.insert(normalized.clone());
    discovered.push(normalized);
}

fn recursive_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut entries = match std::fs::read_dir(root) {
        Ok(entries) => entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>(),
        Err(_) => return dirs,
    };
    entries.sort();
    for entry in entries {
        dirs.push(entry.clone());
        dirs.extend(recursive_dirs(&entry));
    }
    dirs
}
