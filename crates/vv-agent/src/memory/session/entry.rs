use serde::{Deserialize, Serialize};

pub(super) const SESSION_MEMORY_CATEGORIES: &[&str] = &[
    "user_intent",
    "decision",
    "file_change",
    "error_fix",
    "key_fact",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMemoryEntry {
    pub category: String,
    pub content: String,
    pub source_cycle: i32,
    pub importance: u8,
}

impl SessionMemoryEntry {
    pub fn new(
        category: impl Into<String>,
        content: impl Into<String>,
        source_cycle: i32,
        importance: u8,
    ) -> Self {
        Self::normalized(category.into(), content.into(), source_cycle, importance)
    }

    fn normalized(category: String, content: String, source_cycle: i32, importance: u8) -> Self {
        let normalized_category = normalize_category(&category);
        Self {
            category: normalized_category,
            content: content.trim().to_string(),
            source_cycle,
            importance: importance.clamp(1, 10),
        }
    }
}

fn normalize_category(category: &str) -> String {
    let category = category.trim().to_ascii_lowercase();
    if SESSION_MEMORY_CATEGORIES.contains(&category.as_str()) {
        category
    } else {
        "key_fact".to_string()
    }
}

pub(super) fn entry_key(entry: &SessionMemoryEntry) -> (String, String) {
    (
        entry.category.clone(),
        entry
            .content
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase(),
    )
}
