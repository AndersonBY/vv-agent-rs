use super::identifiers::is_identifier_continue;
use super::SettingsLiteralError;

pub(super) fn extract_assignment_literal<'a>(
    source: &'a str,
    targets: &[&str],
) -> Result<&'a str, SettingsLiteralError> {
    for target in targets {
        if let Some(value_start) = find_top_level_assignment_value(source, target) {
            return collect_balanced_literal(&source[value_start..]);
        }
    }
    Err(SettingsLiteralError::MissingAssignment)
}

fn find_top_level_assignment_value(source: &str, target: &str) -> Option<usize> {
    let mut line_start = 0usize;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let indent_len = line.len().saturating_sub(trimmed.len());
        if indent_len == 0 && starts_with_name(trimmed, target) {
            let after_name = &trimmed[target.len()..];
            let next = after_name.chars().next();
            if matches!(next, Some(':') | Some('=')) || next.is_some_and(|ch| ch.is_whitespace()) {
                if let Some(eq_index) = trimmed.find('=') {
                    return Some(line_start + indent_len + eq_index + 1);
                }
            }
        }
        line_start += line.len();
    }
    None
}

fn starts_with_name(source: &str, name: &str) -> bool {
    source.starts_with(name)
        && source[name.len()..]
            .chars()
            .next()
            .is_none_or(|ch| !is_identifier_continue(ch))
}

fn collect_balanced_literal(source: &str) -> Result<&str, SettingsLiteralError> {
    let start = first_literal_char(source).ok_or_else(|| {
        SettingsLiteralError::InvalidLiteral("assignment has no literal value".to_string())
    })?;
    let tail = &source[start..];
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut started = false;
    let mut in_comment = false;

    for (index, ch) in tail.char_indices() {
        if in_comment {
            if matches!(ch, '\n' | '\r') {
                in_comment = false;
            }
            continue;
        }

        if let Some(active_quote) = quote {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == active_quote {
                quote = None;
                if started && depth == 0 {
                    let end = index + ch.len_utf8();
                    return Ok(tail[..end].trim());
                }
            }
            continue;
        }

        match ch {
            '\'' | '"' => {
                started = true;
                quote = Some(ch);
            }
            '#' => {
                in_comment = true;
            }
            '{' | '[' | '(' => {
                started = true;
                depth += 1;
            }
            '}' | ']' | ')' => {
                if depth == 0 {
                    return Err(SettingsLiteralError::InvalidLiteral(
                        "unbalanced closing delimiter".to_string(),
                    ));
                }
                depth -= 1;
                if started && depth == 0 {
                    let end = index + ch.len_utf8();
                    return Ok(tail[..end].trim());
                }
            }
            _ if !ch.is_whitespace() => {
                started = true;
                if depth == 0 {
                    let end = tail[index..]
                        .find(['\n', '\r'])
                        .map(|offset| index + offset)
                        .unwrap_or(tail.len());
                    return Ok(tail[..end].trim());
                }
            }
            _ => {}
        }
    }

    Err(SettingsLiteralError::InvalidLiteral(
        "unterminated literal assignment".to_string(),
    ))
}

fn first_literal_char(source: &str) -> Option<usize> {
    let mut in_comment = false;
    for (index, ch) in source.char_indices() {
        if in_comment {
            if matches!(ch, '\n' | '\r') {
                in_comment = false;
            }
            continue;
        }
        if ch == '#' {
            in_comment = true;
            continue;
        }
        if !ch.is_whitespace() {
            return Some(index);
        }
    }
    None
}
