use super::super::platform::normalize_shell_name;

const WINDOWS_DEFAULT_SHELL_PRIORITY: [&str; 1] = ["cmd"];

pub(super) fn normalize_windows_priority(raw: Option<&[String]>) -> Vec<String> {
    let Some(raw) = raw.filter(|items| !items.is_empty()) else {
        return default_windows_priority();
    };
    let mut normalized = Vec::new();
    for item in raw {
        let value = normalize_shell_name(item);
        if value.is_empty() || normalized.iter().any(|seen| seen == &value) {
            continue;
        }
        normalized.push(value);
    }
    if normalized.is_empty() {
        default_windows_priority()
    } else {
        normalized
    }
}

fn default_windows_priority() -> Vec<String> {
    WINDOWS_DEFAULT_SHELL_PRIORITY
        .iter()
        .map(|item| item.to_string())
        .collect()
}
