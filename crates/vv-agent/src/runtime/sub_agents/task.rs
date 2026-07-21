use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::prompt::{
    build_raw_system_prompt_sections, build_system_prompt_bundle_with_options,
    BuildSystemPromptOptions,
};
use crate::runtime::tool_planner::project_tool_policy;
use crate::types::{AgentTask, NoToolPolicy, SubAgentConfig, SubTaskRequest};

use super::types::{SubRunLifecycle, SubTaskBuildInputs, SubTaskRunContext};
use super::RESERVED_SUB_AGENT_METADATA_KEYS;

pub(super) fn build_sub_agent_task(
    context: &SubTaskRunContext,
    inputs: SubTaskBuildInputs<'_>,
) -> AgentTask {
    let parent_task = &context.parent_task;
    let sub_agent = inputs.sub_agent;
    let request = inputs.request;
    let (system_prompt, generated_sections) = if let Some(system_prompt) = &sub_agent.system_prompt
    {
        (
            system_prompt.clone(),
            build_raw_system_prompt_sections(system_prompt),
        )
    } else {
        let language = parent_task
            .metadata
            .get("language")
            .and_then(Value::as_str)
            .unwrap_or("zh-CN")
            .to_string();
        let available_skills = parent_task
            .metadata
            .get("available_skills")
            .filter(|value| value.is_array())
            .cloned();
        let prompt_bundle = build_system_prompt_bundle_with_options(
            &sub_agent.description,
            BuildSystemPromptOptions {
                language,
                allow_interruption: false,
                use_workspace: parent_task.use_workspace,
                enable_todo_management: true,
                agent_type: parent_task.agent_type.clone(),
                available_skills,
                workspace: Some(context.workspace_path.clone()),
                ..BuildSystemPromptOptions::default()
            },
        );
        (prompt_bundle.prompt, prompt_bundle.sections)
    };
    let mut user_prompt = request.task_description.clone();
    if !request.output_requirements.is_empty() {
        user_prompt.push_str("\n\n<Output Requirements>\n");
        user_prompt.push_str(&request.output_requirements);
        user_prompt.push_str("\n</Output Requirements>");
    }
    if request.include_main_summary {
        let parent_summary = build_parent_summary(parent_task, &context.parent_shared_state);
        if !parent_summary.is_empty() {
            user_prompt.push_str("\n\n<Main Task Summary>\n");
            user_prompt.push_str(&parent_summary);
            user_prompt.push_str("\n</Main Task Summary>");
        }
    }

    let mut sub_task = AgentTask::new(
        &inputs.lifecycle.task_id,
        inputs.resolved_model_id.to_string(),
        system_prompt,
        user_prompt,
    );
    sub_task.max_cycles = sub_agent.max_cycles.max(1);
    sub_task.memory_compact_threshold = parent_task.memory_compact_threshold;
    sub_task.memory_threshold_percentage = parent_task.memory_threshold_percentage;
    sub_task.no_tool_policy = NoToolPolicy::Continue;
    sub_task.allow_interruption = false;
    sub_task.use_workspace = parent_task.use_workspace;
    sub_task.has_sub_agents = false;
    sub_task.sub_agents = BTreeMap::new();
    sub_task.agent_type = parent_task.agent_type.clone();
    sub_task.native_multimodal = inputs.resolved_native_multimodal;
    sub_task.extra_tool_names = parent_task.extra_tool_names.clone();
    sub_task.exclude_tools = merged_sub_task_exclusions(parent_task, sub_agent);
    sub_task.model_settings = parent_task.model_settings.clone();
    sub_task.metadata = build_sub_task_metadata(
        parent_task,
        inputs.lifecycle,
        request,
        &context.workspace_path,
        generated_sections,
    );
    if let Some(context_length) = inputs.resolved_context_length {
        sub_task
            .metadata
            .entry("model_context_window".to_string())
            .or_insert_with(|| Value::from(context_length));
    }
    if let Some(max_output_tokens) = inputs.resolved_max_output_tokens {
        sub_task
            .metadata
            .entry("model_max_output_tokens".to_string())
            .or_insert_with(|| Value::from(max_output_tokens));
    }
    let mut effective_policy = context.tool_policy.clone().unwrap_or_default();
    effective_policy.extend_metadata_denials(&sub_agent.declared_tool_policy());
    project_tool_policy(&mut sub_task, &effective_policy);
    sub_task
}

fn merged_sub_task_exclusions(parent_task: &AgentTask, sub_agent: &SubAgentConfig) -> Vec<String> {
    let mut excluded = parent_task.exclude_tools.clone();
    excluded.extend(sub_agent.exclude_tools.clone());
    excluded.push(crate::constants::CREATE_SUB_TASK_TOOL_NAME.to_string());
    excluded.push(crate::constants::SUB_TASK_STATUS_TOOL_NAME.to_string());
    excluded.sort();
    excluded.dedup();
    excluded
}

fn build_sub_task_metadata(
    parent_task: &AgentTask,
    lifecycle: &SubRunLifecycle,
    request: &SubTaskRequest,
    workspace_path: &Path,
    system_prompt_sections: Vec<Value>,
) -> BTreeMap<String, Value> {
    let mut metadata = BTreeMap::from([
        ("is_sub_task".to_string(), Value::Bool(true)),
        (
            "parent_task_id".to_string(),
            Value::String(parent_task.task_id.clone()),
        ),
        (
            "sub_agent_name".to_string(),
            Value::String(lifecycle.agent_name.clone()),
        ),
        ("session_memory_enabled".to_string(), Value::Bool(false)),
        (
            "workspace".to_string(),
            Value::String(workspace_path.display().to_string()),
        ),
    ]);
    for key in [
        "bash_shell",
        "windows_shell_priority",
        "bash_env",
        "allow_outside_workspace_paths",
        "allow_outside_workspace",
        "workspace_allow_outside_main",
        "workspace_allow_outside",
        "language",
        "available_skills",
        "active_skills",
    ] {
        if let Some(value) = parent_task.metadata.get(key) {
            metadata.insert(key.to_string(), value.clone());
        }
    }
    if let Some(sub_agent) = parent_task.sub_agents.get(&lifecycle.agent_name) {
        metadata.extend(sub_agent.metadata.clone());
    }
    metadata.extend(request.metadata.clone());
    for key in RESERVED_SUB_AGENT_METADATA_KEYS {
        metadata.remove(key);
    }
    if !system_prompt_sections.is_empty() {
        metadata
            .entry("system_prompt_sections".to_string())
            .or_insert(Value::Array(system_prompt_sections));
    }
    metadata.extend(BTreeMap::from([
        ("is_sub_task".to_string(), Value::Bool(true)),
        (
            "parent_task_id".to_string(),
            Value::String(parent_task.task_id.clone()),
        ),
        (
            "sub_agent_name".to_string(),
            Value::String(lifecycle.agent_name.clone()),
        ),
        ("session_memory_enabled".to_string(), Value::Bool(false)),
        (
            "workspace".to_string(),
            Value::String(workspace_path.display().to_string()),
        ),
        (
            "task_id".to_string(),
            Value::String(lifecycle.task_id.clone()),
        ),
        (
            "session_id".to_string(),
            Value::String(lifecycle.session_id.clone()),
        ),
        (
            "browser_scope_key".to_string(),
            Value::String(lifecycle.session_id.clone()),
        ),
    ]));
    canonical_sub_run_metadata(&metadata, lifecycle)
}

pub(super) fn canonical_sub_run_metadata(
    metadata: &BTreeMap<String, Value>,
    lifecycle: &SubRunLifecycle,
) -> BTreeMap<String, Value> {
    let mut canonical = metadata.clone();
    for key in [
        "_vv_agent_agent_name",
        "_vv_agent_parent_run_id",
        "_vv_agent_parent_tool_call_id",
        "_vv_agent_run_id",
        "_vv_agent_session_id",
        "_vv_agent_trace_id",
        "browser_scope_key",
        "parent_run_id",
        "parent_tool_call_id",
        "run_id",
        "session_id",
        "sub_agent_name",
        "task_id",
        "trace_id",
    ] {
        canonical.remove(key);
    }
    canonical.extend(BTreeMap::from([
        (
            "task_id".to_string(),
            Value::String(lifecycle.task_id.clone()),
        ),
        (
            "session_id".to_string(),
            Value::String(lifecycle.session_id.clone()),
        ),
        (
            "browser_scope_key".to_string(),
            Value::String(lifecycle.session_id.clone()),
        ),
        (
            "sub_agent_name".to_string(),
            Value::String(lifecycle.agent_name.clone()),
        ),
        (
            "run_id".to_string(),
            Value::String(lifecycle.run_id.clone()),
        ),
        (
            "trace_id".to_string(),
            Value::String(lifecycle.trace_id.clone()),
        ),
        (
            "_vv_agent_run_id".to_string(),
            Value::String(lifecycle.run_id.clone()),
        ),
        (
            "_vv_agent_trace_id".to_string(),
            Value::String(lifecycle.trace_id.clone()),
        ),
        (
            "_vv_agent_agent_name".to_string(),
            Value::String(lifecycle.agent_name.clone()),
        ),
        (
            "_vv_agent_session_id".to_string(),
            Value::String(lifecycle.session_id.clone()),
        ),
    ]));
    if !lifecycle.parent_run_id.is_empty() {
        canonical.insert(
            "parent_run_id".to_string(),
            Value::String(lifecycle.parent_run_id.clone()),
        );
        canonical.insert(
            "_vv_agent_parent_run_id".to_string(),
            Value::String(lifecycle.parent_run_id.clone()),
        );
    }
    if !lifecycle.parent_tool_call_id.is_empty() {
        canonical.insert(
            "parent_tool_call_id".to_string(),
            Value::String(lifecycle.parent_tool_call_id.clone()),
        );
        canonical.insert(
            "_vv_agent_parent_tool_call_id".to_string(),
            Value::String(lifecycle.parent_tool_call_id.clone()),
        );
    }
    canonical
}

fn build_parent_summary(
    parent_task: &AgentTask,
    parent_shared_state: &BTreeMap<String, Value>,
) -> String {
    let mut lines = vec![format!("Parent task goal: {}", parent_task.user_prompt)];
    if let Some(todo_list) = parent_shared_state
        .get("todo_list")
        .and_then(Value::as_array)
    {
        if !todo_list.is_empty() {
            lines.push("Parent TODO status:".to_string());
            for item in todo_list {
                let title = item
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("Untitled");
                let status = item
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("pending");
                lines.push(format!("- [{status}] {title}"));
            }
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod parity_tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use serde_json::{json, Value};

    use super::build_sub_agent_task;
    use crate::llm::ScriptedLlmClient;
    use crate::model_settings::ModelSettings;
    use crate::runtime::sub_task_manager::SubTaskManager;
    use crate::tools::build_default_registry;
    use crate::types::{AgentTask, SubAgentConfig, SubTaskRequest};
    use crate::workspace::MemoryWorkspaceBackend;

    use super::super::types::{SubRunLifecycle, SubTaskBuildInputs, SubTaskRunContext};

    fn contract() -> Value {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/parity/configured_sub_agent_v1.json"
        )))
        .expect("configured sub-agent parity fixture")
    }

    #[test]
    fn configured_sub_agent_task_and_metadata_match_shared_fixture() {
        let mut parent = AgentTask::new(
            "parent-task",
            "parent-model",
            "Parent prompt",
            "Parent task",
        );
        parent.max_cycles = 6;
        parent.memory_compact_threshold = 64_000;
        parent.memory_threshold_percentage = 80;
        parent.agent_type = Some("computer".to_string());
        parent.extra_tool_names = vec!["custom_tool".to_string()];
        parent.exclude_tools = vec!["parent_excluded".to_string()];
        parent.model_settings = Some(ModelSettings {
            temperature: Some(0.25),
            max_tokens: Some(512),
            ..ModelSettings::default()
        });
        parent.metadata = BTreeMap::from([
            ("language".to_string(), json!("en-US")),
            ("available_skills".to_string(), json!([{"name": "review"}])),
            ("active_skills".to_string(), json!(["review"])),
            ("bash_shell".to_string(), json!("bash")),
        ]);
        let mut child = SubAgentConfig::new("child-model", "Research facts");
        child.system_prompt = Some("Child prompt".to_string());
        child.max_cycles = 4;
        child.exclude_tools = vec!["sub_excluded".to_string()];
        child
            .metadata
            .insert("sub_config_value".to_string(), json!("sub"));
        parent
            .sub_agents
            .insert("researcher".to_string(), child.clone());
        let mut request = SubTaskRequest::new("researcher", "Collect facts");
        request.output_requirements = "Return JSON".to_string();
        let fixture = contract();
        for key in fixture["reserved_metadata_keys"]
            .as_array()
            .expect("reserved metadata keys")
            .iter()
            .filter_map(Value::as_str)
        {
            child
                .metadata
                .insert(key.to_string(), json!(format!("sub-agent-override-{key}")));
        }
        parent
            .sub_agents
            .insert("researcher".to_string(), child.clone());
        request.metadata = BTreeMap::from([("request_value".to_string(), json!("request"))]);
        for key in fixture["reserved_metadata_keys"]
            .as_array()
            .expect("reserved metadata keys")
            .iter()
            .filter_map(Value::as_str)
        {
            request
                .metadata
                .insert(key.to_string(), json!(format!("request-override-{key}")));
        }
        let context = SubTaskRunContext {
            llm_client: Arc::new(ScriptedLlmClient::new(Vec::new())),
            tool_registry: build_default_registry(),
            workspace_backend: Arc::new(MemoryWorkspaceBackend::default()),
            workspace_path: PathBuf::from("/contract-workspace"),
            parent_task: parent,
            parent_shared_state: BTreeMap::new(),
            sub_task_manager: SubTaskManager::default(),
            parent_cancellation_token: None,
            settings_file: None,
            default_backend: None,
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
        let task = build_sub_agent_task(
            &context,
            SubTaskBuildInputs {
                lifecycle: &SubRunLifecycle {
                    run_id: "child-run".to_string(),
                    trace_id: "trace-parity".to_string(),
                    parent_run_id: "parent-run".to_string(),
                    parent_tool_call_id: "delegate".to_string(),
                    task_id: "child-task".to_string(),
                    session_id: "child-session".to_string(),
                    agent_name: "researcher".to_string(),
                    parent_task_id: "parent-task".to_string(),
                    model: "child-model".to_string(),
                },
                sub_agent: &child,
                resolved_model_id: "child-model",
                resolved_native_multimodal: true,
                resolved_context_length: Some(32_000),
                resolved_max_output_tokens: Some(4_096),
                request: &request,
            },
        );
        assert_eq!(task.metadata["model_context_window"], json!(32_000));
        assert_eq!(task.metadata["model_max_output_tokens"], json!(4_096));
        assert!(!task.metadata.contains_key("reserved_output_tokens"));
        let projection = json!({
            "model": task.model,
            "user_prompt": task.user_prompt,
            "max_cycles": task.max_cycles,
            "memory_compact_threshold": task.memory_compact_threshold,
            "memory_threshold_percentage": task.memory_threshold_percentage,
            "no_tool_policy": task.no_tool_policy,
            "allow_interruption": task.allow_interruption,
            "use_workspace": task.use_workspace,
            "has_sub_agents": task.has_sub_agents,
            "agent_type": task.agent_type,
            "native_multimodal": task.native_multimodal,
            "extra_tool_names": task.extra_tool_names,
            "exclude_tools": task.exclude_tools,
            "model_settings": task.model_settings,
            "initial_messages": task.initial_messages,
            "initial_shared_state": task.initial_shared_state,
        });
        let metadata_contract = fixture["metadata_projection"]
            .as_object()
            .expect("metadata projection object");
        let metadata_projection = metadata_contract
            .keys()
            .map(|key| {
                let value = if key == "workspace" {
                    json!("<workspace>")
                } else {
                    task.metadata.get(key).cloned().unwrap_or(Value::Null)
                };
                (key.clone(), value)
            })
            .collect::<serde_json::Map<_, _>>();

        assert_eq!(projection, fixture["task_projection"]);
        assert_eq!(
            Value::Object(metadata_projection),
            fixture["metadata_projection"]
        );
        for key in fixture["reserved_metadata_keys"]
            .as_array()
            .expect("reserved metadata keys")
            .iter()
            .filter_map(Value::as_str)
        {
            if matches!(
                key,
                "_vv_agent_allowed_tools"
                    | "_vv_agent_disallowed_tools"
                    | "_vv_agent_tool_policy_approval"
                    | "_vv_agent_tool_policy_can_use_tool"
            ) {
                assert!(
                    !task.metadata.contains_key(key),
                    "optional policy metadata {key} must be absent without a trusted parent policy"
                );
                continue;
            }
            let expected = if key == "workspace" {
                json!("/contract-workspace")
            } else {
                fixture["metadata_projection"][key].clone()
            };
            assert_eq!(task.metadata[key], expected, "reserved metadata {key}");
        }
    }
}
