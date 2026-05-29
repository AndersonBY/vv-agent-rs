use crate::config::{EndpointConfig, EndpointOption};
use crate::llm::NamedEndpointClientSpec;

use super::ConfigError;

pub(super) fn endpoint_options_from_vv_llm(
    resolved: &vv_llm::ResolvedModelConfig,
    settings: &vv_llm::LlmSettings,
) -> Vec<EndpointOption> {
    let mut endpoint_options = resolved
        .model
        .endpoints
        .iter()
        .filter(|binding| binding.enabled())
        .filter_map(|binding| {
            let endpoint = settings
                .endpoints
                .iter()
                .find(|endpoint| endpoint.id == binding.endpoint_id())?;
            Some(EndpointOption {
                endpoint: endpoint_config_from_vv_llm(endpoint.clone()),
                model_id: binding.model_id(&resolved.model.id).to_string(),
            })
        })
        .collect::<Vec<_>>();

    if endpoint_options.is_empty() {
        endpoint_options.push(EndpointOption {
            endpoint: endpoint_config_from_vv_llm(resolved.endpoint.clone()),
            model_id: resolved.model_id.clone(),
        });
    }

    endpoint_options
}

pub(super) fn build_endpoint_chat_clients(
    resolved: &vv_llm::ResolvedModelConfig,
    settings: &vv_llm::LlmSettings,
) -> Result<Vec<NamedEndpointClientSpec>, ConfigError> {
    endpoint_resolutions_from_vv_llm(resolved, settings)
        .into_iter()
        .map(|endpoint_resolved| {
            let endpoint_id = endpoint_resolved.endpoint.id.clone();
            let model_id = endpoint_resolved.model_id.clone();
            let chat_client = vv_llm::create_chat_client_from_resolved(endpoint_resolved)
                .map_err(|error| ConfigError::InvalidSettings(error.to_string()))?;
            Ok((endpoint_id, model_id, chat_client))
        })
        .collect()
}

fn endpoint_resolutions_from_vv_llm(
    resolved: &vv_llm::ResolvedModelConfig,
    settings: &vv_llm::LlmSettings,
) -> Vec<vv_llm::ResolvedModelConfig> {
    let mut resolutions = resolved
        .model
        .endpoints
        .iter()
        .filter(|binding| binding.enabled())
        .filter_map(|binding| {
            let endpoint = settings
                .endpoints
                .iter()
                .find(|endpoint| endpoint.id == binding.endpoint_id())?;
            Some(vv_llm::ResolvedModelConfig {
                backend: resolved.backend.clone(),
                model: resolved.model.clone(),
                model_id: binding.model_id(&resolved.model.id).to_string(),
                endpoint: endpoint.clone(),
            })
        })
        .collect::<Vec<_>>();

    if resolutions.is_empty() {
        resolutions.push(resolved.clone());
    }

    resolutions
}

pub(super) fn endpoint_config_from_vv_llm(endpoint: vv_llm::EndpointConfig) -> EndpointConfig {
    EndpointConfig {
        endpoint_id: endpoint.id,
        api_key: endpoint.api_key.unwrap_or_default(),
        api_base: endpoint.api_base.unwrap_or_default(),
        endpoint_type: endpoint
            .endpoint_type
            .unwrap_or_else(|| "default".to_string()),
    }
}
