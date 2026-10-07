pub(super) fn normalize_summary_output(text: &str) -> String {
    let mut cleaned = strip_markdown_code_fence(text);
    let analysis_pattern =
        regex::Regex::new(r"(?is)<analysis>.*?</analysis>").expect("analysis regex");
    cleaned = analysis_pattern
        .replace_all(&cleaned, "")
        .trim()
        .to_string();
    let summary_pattern =
        regex::Regex::new(r"(?is)<summary>\s*(.*?)\s*</summary>").expect("summary regex");
    if let Some(captures) = summary_pattern.captures(&cleaned) {
        return captures
            .get(1)
            .map(|matched| matched.as_str().trim().to_string())
            .unwrap_or_default();
    }
    cleaned
}

fn strip_markdown_code_fence(text: &str) -> String {
    let cleaned = text.trim();
    if !cleaned.starts_with("```") {
        return cleaned.to_string();
    }
    let mut lines = cleaned.lines().collect::<Vec<_>>();
    if lines.len() < 2 {
        return cleaned.to_string();
    }
    lines.remove(0);
    if lines
        .last()
        .is_some_and(|line| line.trim().starts_with("```"))
    {
        lines.pop();
    }
    lines.join("\n").trim().to_string()
}
