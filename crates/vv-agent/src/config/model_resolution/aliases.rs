const MODEL_ALIAS_MAP: &[(&str, &str)] = &[("kimi-k2.5", "kimi-k2-thinking")];

pub(super) fn select_model_alias(
    settings: &vv_llm::LlmSettings,
    backend: &str,
    model: &str,
) -> String {
    if settings
        .backends
        .get(backend)
        .is_some_and(|config| config.models.contains_key(model))
    {
        return model.to_string();
    }
    MODEL_ALIAS_MAP
        .iter()
        .find(|(alias, _)| *alias == model)
        .map(|(_, target)| target.to_string())
        .unwrap_or_else(|| model.to_string())
}
