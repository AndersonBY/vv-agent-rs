pub(in crate::llm::vv_llm_client) fn endpoint_type_for_prompt_cache(
    backend: &str,
    provider_name: &str,
) -> String {
    let normalized_provider = provider_name.trim().to_ascii_lowercase();
    if matches!(
        normalized_provider.as_str(),
        "anthropic" | "anthropic_vertex"
    ) {
        return normalized_provider;
    }
    backend.trim().to_ascii_lowercase()
}
