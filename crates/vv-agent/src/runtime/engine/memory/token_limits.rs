use std::path::Path;

use crate::memory::token_utils::resolve_model_token_limits_from_file;

pub(super) fn resolve_runtime_model_token_limits(
    settings_file: Option<&Path>,
    default_backend: Option<&str>,
    model: &str,
) -> (Option<u64>, Option<u64>) {
    let (Some(settings_file), Some(default_backend)) = (settings_file, default_backend) else {
        return (None, None);
    };
    resolve_model_token_limits_from_file(settings_file, default_backend, model)
}
