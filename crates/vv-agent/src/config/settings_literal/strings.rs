use super::SettingsLiteralError;

pub(super) fn parse_literal_string(
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
