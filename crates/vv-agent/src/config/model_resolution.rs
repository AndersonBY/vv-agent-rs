use std::path::Path;

use serde_json::Value;

use crate::llm::VvLlmClient;

use super::{load_llm_settings_from_file, ConfigError, ResolvedModelConfig};

mod aliases;
mod backend;
mod endpoints;
mod settings;

use aliases::select_model_alias;
use backend::backend_type_from_str;
use endpoints::{build_endpoint_chat_clients, endpoint_options_from_vv_llm};
use settings::normalize_llm_settings;

pub use settings::build_vv_llm_settings;

pub fn resolve_model_endpoint(
    settings: &Value,
    backend: &str,
    model: &str,
) -> Result<ResolvedModelConfig, ConfigError> {
    let settings = normalize_llm_settings(settings)?;
    let backend_type = backend_type_from_str(backend)?;
    let selected_model = select_model_alias(&settings, backend, model);
    let resolved = settings
        .resolve_chat_model(backend_type, &selected_model)
        .map_err(|error| ConfigError::InvalidSettings(error.to_string()))?;
    Ok(resolved_from_vv_llm(
        backend,
        model,
        &selected_model,
        resolved,
        &settings,
    ))
}

pub fn build_vv_llm_from_local_settings(
    settings_path: impl AsRef<Path>,
    backend: &str,
    model: &str,
    timeout_seconds: f64,
) -> Result<(VvLlmClient, ResolvedModelConfig), ConfigError> {
    let settings_value = load_llm_settings_from_file(settings_path)?;
    let settings = normalize_llm_settings(&settings_value)?;
    let backend_type = backend_type_from_str(backend)?;
    let selected_model = select_model_alias(&settings, backend, model);
    let vv_resolved = settings
        .resolve_chat_model(backend_type, &selected_model)
        .map_err(|error| ConfigError::InvalidSettings(error.to_string()))?;
    let resolved = resolved_from_vv_llm(
        backend,
        model,
        &selected_model,
        vv_resolved.clone(),
        &settings,
    );
    let endpoint_clients = build_endpoint_chat_clients(&vv_resolved, &settings)?;
    let llm = VvLlmClient::new_with_named_endpoint_clients(
        backend,
        resolved.selected_model.clone(),
        resolved.model_id.clone(),
        endpoint_clients,
        timeout_seconds,
    );
    Ok((llm, resolved))
}

fn resolved_from_vv_llm(
    backend: &str,
    requested_model: &str,
    selected_model: &str,
    resolved: vv_llm::ResolvedModelConfig,
    settings: &vv_llm::LlmSettings,
) -> ResolvedModelConfig {
    let endpoint_options = endpoint_options_from_vv_llm(&resolved, settings);
    ResolvedModelConfig::new(
        backend,
        requested_model,
        selected_model,
        resolved.model_id.clone(),
        endpoint_options,
    )
    .with_token_limits(
        resolved.model.context_length.map(u64::from),
        resolved.model.max_output_tokens.map(u64::from),
    )
}
