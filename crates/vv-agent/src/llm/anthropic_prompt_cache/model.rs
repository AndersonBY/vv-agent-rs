pub(super) fn minimum_cacheable_tokens(model: &str) -> usize {
    if model.contains("opus-4-6") || model.contains("opus-4-5") {
        return 4096;
    }
    if model.contains("haiku-4-5") {
        return 4096;
    }
    if model.contains("haiku") {
        return 2048;
    }
    1024
}
