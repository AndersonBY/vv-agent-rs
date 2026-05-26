use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::memory::token_utils::estimate_tokens;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostCompactRestoreConfig {
    pub total_budget_tokens: u64,
    pub max_tokens_per_file: u64,
    pub max_files: usize,
    pub token_model: String,
}

impl Default for PostCompactRestoreConfig {
    fn default() -> Self {
        Self {
            total_budget_tokens: 30_000,
            max_tokens_per_file: 5_000,
            max_files: 10,
            token_model: String::new(),
        }
    }
}

pub fn restore_key_files(
    summary_data: &Value,
    workspace: Option<&Path>,
    config: &PostCompactRestoreConfig,
) -> String {
    let Some(workspace) = workspace else {
        return String::new();
    };
    let Some(raw_files) = summary_data
        .get("files_examined_or_modified")
        .and_then(Value::as_array)
    else {
        return String::new();
    };
    let Ok(workspace_root) = workspace.canonicalize() else {
        return String::new();
    };

    let mut indexed_files = raw_files
        .iter()
        .enumerate()
        .filter_map(|(index, item)| item.as_object().map(|file| (index, file)))
        .collect::<Vec<_>>();
    indexed_files.sort_by_key(|(index, file)| {
        let action = file
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("read")
            .trim()
            .to_ascii_lowercase();
        (action_priority(&action), *index)
    });

    let mut restored_parts = Vec::new();
    let mut total_tokens = 0_u64;
    for (_, file_info) in indexed_files.into_iter().take(config.max_files) {
        let path_value = file_info
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        if path_value.is_empty() {
            continue;
        }
        let Some(resolved_path) = resolve_workspace_file(&workspace_root, path_value) else {
            continue;
        };
        if !resolved_path.is_file() {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&resolved_path) else {
            continue;
        };

        let action = file_info
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("read")
            .trim()
            .to_ascii_lowercase();
        let action = if action.is_empty() { "read" } else { &action };
        let content = truncate_to_token_budget(
            &content,
            config.max_tokens_per_file.max(1),
            &config.token_model,
        );
        let candidate =
            format!("<file path=\"{path_value}\" action=\"{action}\">\n{content}\n</file>");
        let candidate_tokens = estimate_tokens(&candidate, &config.token_model);
        if total_tokens + candidate_tokens > config.total_budget_tokens {
            break;
        }
        restored_parts.push(candidate);
        total_tokens += candidate_tokens;
    }

    if restored_parts.is_empty() {
        return String::new();
    }
    format!(
        "<Post-Compaction File Context>\nThe following files were relevant in the previous conversation context:\n\n{}\n</Post-Compaction File Context>",
        restored_parts.join("\n\n")
    )
}

fn action_priority(action: &str) -> u8 {
    match action {
        "modified" => 0,
        "created" => 1,
        "deleted" => 2,
        "read" => 3,
        _ => 99,
    }
}

fn resolve_workspace_file(workspace_root: &Path, relative_path: &str) -> Option<PathBuf> {
    let candidate = workspace_root.join(relative_path).canonicalize().ok()?;
    candidate.starts_with(workspace_root).then_some(candidate)
}

fn truncate_to_token_budget(content: &str, max_tokens: u64, model: &str) -> String {
    if estimate_tokens(content, model) <= max_tokens {
        return content.to_string();
    }

    let notice = "\n... [truncated after compaction restore]";
    let mut low = 0_usize;
    let mut high = content.len();
    let mut best = notice.to_string();
    while low <= high {
        let middle = (low + high) / 2;
        let prefix = floor_char_boundary(content, middle);
        let candidate = format!("{}{}", content[..prefix].trim_end(), notice);
        let candidate_tokens = estimate_tokens(&candidate, model);
        if candidate_tokens <= max_tokens {
            best = candidate;
            low = middle + 1;
        } else if middle == 0 {
            break;
        } else {
            high = middle - 1;
        }
    }
    best
}

fn floor_char_boundary(content: &str, mut index: usize) -> usize {
    index = index.min(content.len());
    while index > 0 && !content.is_char_boundary(index) {
        index -= 1;
    }
    index
}
