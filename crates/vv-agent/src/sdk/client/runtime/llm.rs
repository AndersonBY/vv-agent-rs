use std::path::Path;
use std::sync::Arc;

use crate::config::{build_vv_llm_from_local_settings, ResolvedModelConfig};
use crate::sdk::types::{AgentSDKOptions, SdkLlmClient};

pub(super) fn build_llm_from_options(
    options: &AgentSDKOptions,
    backend: &str,
    model: &str,
) -> Result<(SdkLlmClient, ResolvedModelConfig), String> {
    if let Some(builder) = &options.llm_builder {
        let (mut llm, resolved) = builder(
            options.settings_file.as_path(),
            backend,
            model,
            options.timeout_seconds,
        )?;
        apply_debug_dump_dir_to_llm(&mut llm, options.debug_dump_dir.as_deref());
        return Ok((llm, resolved));
    }
    let (mut llm, resolved) = build_vv_llm_from_local_settings(
        &options.settings_file,
        backend,
        model,
        options.timeout_seconds,
    )
    .map_err(|err| err.to_string())?;
    if let Some(debug_dump_dir) = &options.debug_dump_dir {
        llm = llm.with_debug_dump_dir(debug_dump_dir);
    }
    Ok((Arc::new(llm), resolved))
}

fn apply_debug_dump_dir_to_llm(llm: &mut SdkLlmClient, debug_dump_dir: Option<&str>) {
    let Some(debug_dump_dir) = debug_dump_dir else {
        return;
    };
    let debug_dump_dir = Path::new(debug_dump_dir);
    if let Some(configured_llm) = llm.clone_with_debug_dump_dir(debug_dump_dir) {
        *llm = configured_llm;
    } else {
        llm.set_debug_dump_dir(debug_dump_dir);
    }
}
