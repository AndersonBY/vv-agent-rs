use super::identifiers::{
    is_identifier_start, is_string_prefix, prefixed_string_start, read_identifier,
};
use super::strings::parse_literal_string;
use super::SettingsLiteralError;

pub(super) fn literal_to_json(source: &str) -> Result<String, SettingsLiteralError> {
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
