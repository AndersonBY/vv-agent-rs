use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SettingsLiteralError {
    #[error("cannot find LLM_SETTINGS or settings assignment")]
    MissingAssignment,
    #[error("invalid settings literal: {0}")]
    InvalidLiteral(String),
    #[error("failed to decode normalized settings literal as JSON: {0}")]
    Json(#[from] serde_json::Error),
}

pub(super) fn parse_llm_settings_source(source: &str) -> Result<Value, SettingsLiteralError> {
    let literal = extract_assignment_literal(source, &["LLM_SETTINGS", "settings"])?;
    let json_source = literal_to_json(literal)?;
    let value: Value = serde_json::from_str(&json_source)?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(SettingsLiteralError::InvalidLiteral(
            "settings assignment must evaluate to a mapping".to_string(),
        ))
    }
}

pub(super) fn parse_string_assignment(source: &str, targets: &[&str]) -> Option<String> {
    let literal = extract_assignment_literal(source, targets).ok()?;
    let json_source = literal_to_json(literal).ok()?;
    let value = serde_json::from_str::<Value>(&json_source).ok()?;
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn extract_assignment_literal<'a>(
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

fn literal_to_json(source: &str) -> Result<String, SettingsLiteralError> {
    let mut output = String::with_capacity(source.len());
    let mut index = 0usize;
    while index < source.len() {
        let ch = source[index..].chars().next().ok_or_else(|| {
            SettingsLiteralError::InvalidLiteral("invalid utf-8 boundary".to_string())
        })?;
        match ch {
            '\'' | '"' => {
                let (value, next_index) = parse_literal_string(source, index, false)?;
                output.push_str(&serde_json::to_string(&value)?);
                index = next_index;
            }
            '#' => {
                index = skip_comment(source, index);
            }
            ',' => {
                if !next_non_ws_is_closing(source, index + ch.len_utf8()) {
                    output.push(ch);
                }
                index += ch.len_utf8();
            }
            '(' => {
                output.push('[');
                index += ch.len_utf8();
            }
            ')' => {
                output.push(']');
                index += ch.len_utf8();
            }
            ch if is_identifier_start(ch) => {
                let (word, next_index) = read_identifier(source, index);
                if is_string_prefix(word) {
                    if let Some((quote_index, raw)) =
                        prefixed_string_start(source, next_index, word)
                    {
                        let (value, consumed) = parse_literal_string(source, quote_index, raw)?;
                        output.push_str(&serde_json::to_string(&value)?);
                        index = consumed;
                        continue;
                    }
                }
                match word {
                    "True" => output.push_str("true"),
                    "False" => output.push_str("false"),
                    "None" => output.push_str("null"),
                    other => {
                        return Err(SettingsLiteralError::InvalidLiteral(format!(
                            "unsupported identifier {other:?}"
                        )));
                    }
                }
                index = next_index;
            }
            _ => {
                output.push(ch);
                index += ch.len_utf8();
            }
        }
    }
    Ok(output)
}

fn parse_literal_string(
    source: &str,
    start: usize,
    raw: bool,
) -> Result<(String, usize), SettingsLiteralError> {
    let quote = source[start..].chars().next().ok_or_else(|| {
        SettingsLiteralError::InvalidLiteral("string start is out of bounds".to_string())
    })?;
    let quote_len = quote.len_utf8();
    let triple = source[start..].starts_with(&quote.to_string().repeat(3));
    let content_start = start + if triple { quote_len * 3 } else { quote_len };
    let mut content = String::new();
    let mut index = content_start;

    while index < source.len() {
        if triple && source[index..].starts_with(&quote.to_string().repeat(3)) {
            return Ok((content, index + quote_len * 3));
        }
        let ch = source[index..].chars().next().ok_or_else(|| {
            SettingsLiteralError::InvalidLiteral("invalid string boundary".to_string())
        })?;
        if !triple && ch == quote {
            return Ok((content, index + quote_len));
        }
        if ch == '\\' && !raw {
            let (escaped, next_index) = parse_escape(source, index + ch.len_utf8())?;
            content.push(escaped);
            index = next_index;
        } else {
            content.push(ch);
            index += ch.len_utf8();
        }
    }

    Err(SettingsLiteralError::InvalidLiteral(
        "unterminated string literal".to_string(),
    ))
}

fn parse_escape(source: &str, start: usize) -> Result<(char, usize), SettingsLiteralError> {
    let escaped = source[start..].chars().next().ok_or_else(|| {
        SettingsLiteralError::InvalidLiteral("unterminated escape sequence".to_string())
    })?;
    let next = start + escaped.len_utf8();
    let value = match escaped {
        '\\' => '\\',
        '\'' => '\'',
        '"' => '"',
        'n' => '\n',
        'r' => '\r',
        't' => '\t',
        'b' => '\u{0008}',
        'f' => '\u{000c}',
        'x' => return parse_hex_escape(source, next, 2),
        'u' => return parse_hex_escape(source, next, 4),
        'U' => return parse_hex_escape(source, next, 8),
        other => other,
    };
    Ok((value, next))
}

fn parse_hex_escape(
    source: &str,
    start: usize,
    digits: usize,
) -> Result<(char, usize), SettingsLiteralError> {
    let mut end = start;
    let mut hex = String::new();
    for _ in 0..digits {
        let ch = source[end..].chars().next().ok_or_else(|| {
            SettingsLiteralError::InvalidLiteral("unterminated hex escape".to_string())
        })?;
        if !ch.is_ascii_hexdigit() {
            return Err(SettingsLiteralError::InvalidLiteral(
                "invalid hex escape".to_string(),
            ));
        }
        hex.push(ch);
        end += ch.len_utf8();
    }
    let codepoint = u32::from_str_radix(&hex, 16)
        .map_err(|_| SettingsLiteralError::InvalidLiteral("invalid unicode escape".to_string()))?;
    let value = char::from_u32(codepoint).ok_or_else(|| {
        SettingsLiteralError::InvalidLiteral("invalid unicode codepoint".to_string())
    })?;
    Ok((value, end))
}

fn skip_comment(source: &str, start: usize) -> usize {
    source[start..]
        .find(['\n', '\r'])
        .map(|offset| start + offset)
        .unwrap_or(source.len())
}

fn next_non_ws_is_closing(source: &str, start: usize) -> bool {
    let mut index = start;
    while index < source.len() {
        let ch = source[index..].chars().next().expect("valid char boundary");
        if ch == '#' {
            index = skip_comment(source, index);
            continue;
        }
        if ch.is_whitespace() {
            index += ch.len_utf8();
            continue;
        }
        return matches!(ch, '}' | ']' | ')');
    }
    false
}

fn read_identifier(source: &str, start: usize) -> (&str, usize) {
    let mut end = start;
    for (offset, ch) in source[start..].char_indices() {
        if offset == 0 {
            end = start + ch.len_utf8();
            continue;
        }
        if !is_identifier_continue(ch) {
            return (&source[start..start + offset], start + offset);
        }
        end = start + offset + ch.len_utf8();
    }
    (&source[start..end], end)
}

fn is_identifier_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_identifier_continue(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

fn is_string_prefix(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|ch| matches!(ch.to_ascii_lowercase(), 'r' | 'u' | 'b'))
}

fn prefixed_string_start(source: &str, start: usize, prefix: &str) -> Option<(usize, bool)> {
    let rest = &source[start..];
    let quote_offset = rest
        .char_indices()
        .find(|(_, ch)| !ch.is_whitespace())
        .and_then(|(offset, ch)| matches!(ch, '\'' | '"').then_some(offset))?;
    let raw = prefix
        .chars()
        .any(|ch| matches!(ch.to_ascii_lowercase(), 'r'));
    Some((start + quote_offset, raw))
}
