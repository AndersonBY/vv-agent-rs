use std::collections::BTreeMap;
use std::sync::Arc;

use crate::config::build_vv_llm_from_local_settings;
use crate::types::{AgentTask, SubAgentConfig};

use super::super::types::{ResolvedSubAgentClient, SubTaskRunContext};

pub(super) fn resolve_sub_agent_client(
    context: &SubTaskRunContext,
    parent_task: &AgentTask,
    sub_agent: &SubAgentConfig,
) -> Result<ResolvedSubAgentClient, String> {
    let requested_model = if sub_agent.model.trim().is_empty() {
        parent_task.model.clone()
    } else {
        sub_agent.model.clone()
    };

    if let Some(settings_file) = &context.settings_file {
        let backend = sub_agent
            .backend
            .clone()
            .or_else(|| context.default_backend.clone())
            .unwrap_or_else(|| "inline".to_string());
        let (client, resolved) = build_vv_llm_from_local_settings(
            settings_file,
            &backend,
            &requested_model,
            context.sub_agent_timeout_seconds,
        )
        .map_err(|error| error.to_string())?;
        let endpoint = resolved
            .endpoint()
            .map(|endpoint| endpoint.endpoint_id.clone())
            .unwrap_or_default();
        let resolved_payload = BTreeMap::from([
            ("backend".to_string(), resolved.backend.clone()),
            (
                "selected_model".to_string(),
                resolved.selected_model.clone(),
            ),
            ("model_id".to_string(), resolved.model_id.clone()),
            ("endpoint".to_string(), endpoint),
        ]);
        return Ok(ResolvedSubAgentClient {
            llm_client: Arc::new(client),
            model_id: resolved.model_id,
            payload: resolved_payload,
        });
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
        payload: BTreeMap::new(),
    })
}
