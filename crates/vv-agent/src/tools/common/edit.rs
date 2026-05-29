pub(crate) fn replace_n(
    text: &str,
    old_str: &str,
    new_str: &str,
    max_replacements: usize,
) -> String {
    let mut remaining = text;
    let mut replaced = String::new();
    let mut count = 0;
    while count < max_replacements {
        let Some(index) = remaining.find(old_str) else {
            break;
        };
        replaced.push_str(&remaining[..index]);
        replaced.push_str(new_str);
        remaining = &remaining[index + old_str.len()..];
        count += 1;
    }
    replaced.push_str(remaining);
    replaced
}
