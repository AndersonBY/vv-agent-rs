use std::collections::BTreeMap;
use std::sync::Arc;

use crate::config::build_vv_llm_from_local_settings;
use crate::model::ModelRef;
use crate::tools::common::trim_portable_whitespace;
use crate::types::{AgentTask, SubAgentConfig};

use super::super::types::{ResolvedSubAgentClient, SubTaskRunContext};

pub(super) fn resolve_sub_agent_client(
    context: &SubTaskRunContext,
    parent_task: &AgentTask,
    sub_agent: &SubAgentConfig,
) -> Result<ResolvedSubAgentClient, String> {
    let requested_model = trim_portable_whitespace(&sub_agent.model).to_string();
    let requested_backend = sub_agent
        .backend
        .as_deref()
        .map(trim_portable_whitespace)
        .filter(|backend| !backend.is_empty());
    let run_model_ref = requested_backend
        .map(|backend| ModelRef::backend(backend, &requested_model))
        .unwrap_or_else(|| ModelRef::named(&requested_model));

    if let Some(provider) = &context.model_provider {
        let resolved = provider
            .resolve(&run_model_ref)
            .map_err(|error| error.to_string())?;
        let client = provider
            .client(&resolved)
            .map_err(|error| error.to_string())?;
        let payload = resolved_payload(&resolved);
        return Ok(ResolvedSubAgentClient {
            llm_client: client,
            model_id: resolved.model_id,
            run_model_ref,
            native_multimodal: resolved.native_multimodal,
            context_length: resolved.context_length,
            max_output_tokens: resolved.max_output_tokens,
            payload,
        });
    }

    if let Some(settings_file) = &context.settings_file {
        let backend = requested_backend
            .map(str::to_string)
            .or_else(|| {
                context
                    .default_backend
                    .as_deref()
                    .map(trim_portable_whitespace)
                    .filter(|backend| !backend.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "inline".to_string());
        let (client, resolved) = build_vv_llm_from_local_settings(
            settings_file,
            &backend,
            &requested_model,
            context.sub_agent_timeout_seconds,
        )
        .map_err(|error| error.to_string())?;
        let resolved_payload = resolved_payload(&resolved);
        return Ok(ResolvedSubAgentClient {
            llm_client: Arc::new(client),
            model_id: resolved.model_id,
            run_model_ref,
            native_multimodal: resolved.native_multimodal,
            context_length: resolved.context_length,
            max_output_tokens: resolved.max_output_tokens,
            payload: resolved_payload,
        });
    }

    if requested_backend.is_some() {
        return Err(
            "Sub-agent model resolution requires model_provider or settings_file when backend is explicitly configured."
                .to_string(),
        );
    }

    if requested_model != parent_task.model {
        return Err(
            "Sub-agent model resolution requires runtime settings_file when sub-agent model differs from parent model."
                .to_string(),
        );
    }

    Ok(ResolvedSubAgentClient {
        llm_client: context.llm_client.clone(),
        model_id: parent_task.model.clone(),
        run_model_ref,
        native_multimodal: parent_task.native_multimodal,
        context_length: parent_task
            .metadata
            .get("model_context_window")
            .and_then(serde_json::Value::as_u64),
        max_output_tokens: parent_task
            .metadata
            .get("reserved_output_tokens")
            .and_then(serde_json::Value::as_u64),
        payload: BTreeMap::new(),
    })
}

fn resolved_payload(resolved: &crate::config::ResolvedModelConfig) -> BTreeMap<String, String> {
    let mut payload = BTreeMap::from([
        ("backend".to_string(), resolved.backend.clone()),
        (
            "selected_model".to_string(),
            resolved.selected_model.clone(),
        ),
        ("model_id".to_string(), resolved.model_id.clone()),
    ]);
    if let Some(endpoint) = resolved.endpoint() {
        payload.insert("endpoint".to_string(), endpoint.endpoint_id.clone());
    }
    payload
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::resolve_sub_agent_client;
    use crate::llm::ScriptedLlmClient;
    use crate::runtime::sub_agents::types::SubTaskRunContext;
    use crate::runtime::sub_task_manager::SubTaskManager;
    use crate::tools::build_default_registry;
    use crate::types::{AgentTask, SubAgentConfig};
    use crate::workspace::MemoryWorkspaceBackend;

    #[test]
    fn portable_blank_backend_uses_default_backend_for_settings_resolution() {
        let contract: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/parity/configured_sub_agent_v1.json"
        ))
        .expect("configured sub-agent contract");
        let workspace = tempfile::tempdir().expect("workspace");
        let settings_file = workspace.path().join("settings.json");
        std::fs::write(
            &settings_file,
            json!({
                "VERSION": "2",
                "endpoints": [{
                    "id": "child-endpoint",
                    "api_base": "https://example.invalid/v1",
                    "api_key": "sk-test"
                }],
                "backends": {
                    "moonshot": {
                        "models": {
                            "child-model": {
                                "id": "child-model",
                                "endpoints": [{
                                    "endpoint_id": "child-endpoint",
                                    "model_id": "child-model"
                                }]
                            }
                        }
                    }
                },
                "embedding_backends": {},
                "rerank_backends": {}
            })
            .to_string(),
        )
        .expect("write settings");
        let parent_task =
            AgentTask::new("parent-task", "parent-model", "Parent prompt", "Delegate");
        let mut sub_agent = SubAgentConfig::new("child-model", "Research");
        sub_agent.backend = Some(
            contract["validation"]["portable_whitespace"]["blank_backend_input"]
                .as_str()
                .expect("portable blank backend")
                .to_string(),
        );
        let context = SubTaskRunContext {
            llm_client: Arc::new(ScriptedLlmClient::new(Vec::new())),
            tool_registry: build_default_registry(),
            workspace_backend: Arc::new(MemoryWorkspaceBackend::default()),
            workspace_path: workspace.path().to_path_buf(),
            parent_task: parent_task.clone(),
            parent_shared_state: Default::default(),
            sub_task_manager: SubTaskManager::default(),
            parent_cancellation_token: None,
            settings_file: Some(settings_file),
            default_backend: Some("moonshot".to_string()),
            sub_agent_timeout_seconds: 30.0,
            stream_callback: None,
            parent_log_handler: None,
            parent_event_handler: None,
            parent_execution_context: None,
            model_provider: None,
            parent_run_context: None,
            tool_policy: None,
            budget_limits: None,
        };

        let resolved = resolve_sub_agent_client(&context, &parent_task, &sub_agent)
            .expect("resolve child model through default backend");

        assert_eq!(resolved.model_id, "child-model");
        assert_eq!(resolved.payload["backend"], "moonshot");
        assert_eq!(resolved.payload["endpoint"], "child-endpoint");
        assert_eq!(
            contract["model_resolution"]["blank_backend_treated_as_absent"],
            true
        );
    }
}
