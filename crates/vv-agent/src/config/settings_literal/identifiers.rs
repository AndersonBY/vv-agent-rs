pub(super) fn read_identifier(source: &str, start: usize) -> (&str, usize) {
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

pub(super) fn is_identifier_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

pub(super) fn is_identifier_continue(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}

pub(super) fn is_string_prefix(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|ch| matches!(ch.to_ascii_lowercase(), 'r' | 'u' | 'b'))
}

pub(super) fn prefixed_string_start(
    source: &str,
    start: usize,
    prefix: &str,
) -> Option<(usize, bool)> {
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
