pub(crate) fn replace_n(
    text: &str,
    old_string: &str,
    new_string: &str,
    replacement_limit: usize,
) -> String {
    let mut remaining = text;
    let mut replaced = String::new();
    let mut count = 0;
    while count < replacement_limit {
        let Some(index) = remaining.find(old_string) else {
            break;
        };
        replaced.push_str(&remaining[..index]);
        replaced.push_str(new_string);
        remaining = &remaining[index + old_string.len()..];
        count += 1;
    }
    replaced.push_str(remaining);
    replaced
}
