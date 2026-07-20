use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};

use crate::agent::{Agent, ToolUseBehavior};
use crate::checkpoint::{
    normalize_run_definition, run_definition_comparison_copy, run_definition_digest,
    validate_extension_namespace, CheckpointError, CheckpointResult, ToolIdempotency,
    RUN_DEFINITION_SCHEMA,
};
use crate::config::ResolvedModelConfig;
use crate::constants::{CREATE_SUB_TASK_TOOL_NAME, SUB_TASK_STATUS_TOOL_NAME, WORKSPACE_TOOLS};
use crate::model_settings::{ModelSettings, ResponseFormat};
use crate::run_config::RunConfig;
use crate::runtime::backends::{
    CapabilityRef, DistributedRunEnvelope, ResolvedDistributedCapabilities,
};
use crate::runtime::state_v2::CheckpointV2;
use crate::runtime::tool_planner::plan_tool_schemas;
use crate::tools::{ApprovalPolicy, ApprovalRequirement, ToolApprovalRule, ToolRegistry};
use crate::types::{AgentTask, Message, MessageRole, NoToolPolicy};

mod projection;
mod tool_policy;

use projection::{behavior_metadata, output_schema, require_declared_credential_headers};

pub(crate) use tool_policy::tool_idempotency_for;
use tool_policy::{
    approval_policy_name, no_tool_policy_name, normalized_name_set, stop_at_tool_names,
    tool_use_behavior_name, utf16_cmp,
};

const RUNTIME_METADATA_KEYS: &[&str] = &[
    "_vv_agent_initial_budget_usage",
    "_vv_agent_active_cycle_index",
    "_vv_agent_checkpoint_controller",
    "_vv_agent_checkpoint_budget_snapshot",
    "_vv_agent_run_id",
    "_vv_agent_trace_id",
];

const KNOWN_CREDENTIAL_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "api-key",
];

pub(crate) struct RunDefinitionRequest<'a> {
    pub agent: &'a Agent,
    pub root_input: &'a str,
    pub run_config: &'a RunConfig,
    pub resolved: &'a ResolvedModelConfig,
    pub model_settings: &'a ModelSettings,
    pub task: &'a AgentTask,
    pub registry: &'a ToolRegistry,
    pub initial_messages: &'a [Message],
}

pub(crate) fn build_run_definition(
    request: RunDefinitionRequest<'_>,
) -> CheckpointResult<(Value, String)> {
    let config = request
        .run_config
        .checkpoint_config
        .as_ref()
        .ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_config_invalid",
                "checkpoint_config is required to build a run definition",
            )
        })?;
    config.validate()?;

    let mut refs = config.capability_refs.clone();
    validate_behavior_capability_refs(request.agent, request.run_config, &refs)?;

    let context_ref = take_ref(&mut refs, "context", request.run_config.app_state.is_some())?;
    let workspace_ref = take_ref(
        &mut refs,
        "workspace",
        request.run_config.workspace.is_some() || request.run_config.workspace_backend.is_some(),
    )?;
    let session_ref = take_ref(&mut refs, "session", request.run_config.session.is_some())?;
    let predicate_ref = take_ref(
        &mut refs,
        "tool_policy.predicate",
        request.run_config.tool_policy.can_use_tool.is_some(),
    )?;

    let (settings, transport_timeout_seconds) = model_settings_definition(request.model_settings)?;
    let tools = tool_definitions(request.registry, request.task, &mut refs)?;
    let tool_policy = request
        .run_config
        .tool_policy
        .normalized()
        .map_err(|error| {
            CheckpointError::new("checkpoint_definition_invalid", error.to_string())
        })?;
    let extensions = extension_definitions(request.run_config)?;
    let output_schema = output_schema(request.agent, request.model_settings);

    let mut credential_slots = config.credential_slots.clone();
    credential_slots.sort_by(|left, right| utf16_cmp(left, right));
    credential_slots.dedup();

    let definition = json!({
        "schema_version": RUN_DEFINITION_SCHEMA,
        "agent": {
            "name": request.agent.name(),
            "type": request.task.agent_type,
        },
        "root_input": request.root_input,
        "compiled_prompt": request.task.system_prompt,
        "initial_messages": request.initial_messages.iter().map(Message::to_dict).collect::<Vec<_>>(),
        "initial_shared_state": request.task.initial_shared_state,
        "run_metadata": behavior_metadata(request.agent, request.run_config),
        "context_ref": context_ref,
        "model": {
            "backend": request.resolved.backend,
            "model_id": request.resolved.model_id,
            "settings": settings,
            "transport_timeout_seconds": transport_timeout_seconds,
        },
        "credential_slots": credential_slots,
        "runtime_controls": {
            "max_cycles": request.task.max_cycles,
            "max_handoffs": request.run_config.max_handoffs,
            "no_tool_policy": no_tool_policy_name(request.task.no_tool_policy),
            "memory_compact_threshold": request.task.memory_compact_threshold,
            "memory_threshold_percentage": request.task.memory_threshold_percentage,
            "allow_interruption": request.task.allow_interruption,
            "native_multimodal": request.task.native_multimodal,
            "tool_use_behavior": tool_use_behavior_name(request.agent.tool_use_behavior()),
            "stop_at_tool_names": stop_at_tool_names(request.agent.tool_use_behavior()),
        },
        "tools": tools,
        "tool_policy": {
            "allowed_tools": normalized_name_set(tool_policy.allowed_tools.as_deref()),
            "disallowed_tools": normalized_name_set(Some(&tool_policy.disallowed_tools))
                .unwrap_or_default(),
            "approval": approval_policy_name(tool_policy.approval),
            "predicate_ref": predicate_ref,
            "approval_timeout_seconds": request.run_config.approval_timeout.map(|timeout| timeout.as_secs_f64()),
            "denied_side_effects": tool_policy.denied_side_effects,
            "denied_capability_tags": tool_policy.denied_capability_tags,
            "deny_terminal_tools": tool_policy.deny_terminal_tools,
            "denied_cost_dimensions": tool_policy.denied_cost_dimensions,
        },
        "checkpoint_policy": {
            "ambiguous_model_policy": config.ambiguous_model_policy,
            "ambiguous_tool_policy": config.ambiguous_tool_policy,
            "max_extension_state_bytes": config.max_extension_state_bytes,
        },
        "budget_limits": request
            .run_config
            .budget_limits
            .as_ref()
            .filter(|limits| limits.has_limits()),
        "output_schema": output_schema,
        "workspace_ref": workspace_ref,
        "session_ref": session_ref,
        "extensions": extensions,
        "capability_refs": refs,
    });

    require_declared_credential_headers(&definition, &credential_slots)?;
    let normalized = normalize_run_definition(&definition, &credential_slots)?;
    let digest = run_definition_digest(&normalized)?;
    Ok((normalized, digest))
}

pub(crate) fn build_frozen_task(
    agent: &Agent,
    checkpoint: &CheckpointV2,
    model_settings: &ModelSettings,
) -> CheckpointResult<AgentTask> {
    let definition = checkpoint.run_definition.as_object().ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            "checkpoint run definition must be an object",
        )
    })?;
    let controls = definition
        .get("runtime_controls")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_definition_invalid",
                "checkpoint runtime_controls must be an object",
            )
        })?;
    let model = definition
        .get("model")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_definition_invalid",
                "checkpoint model must be an object",
            )
        })?;
    let agent_definition = definition
        .get("agent")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_definition_invalid",
                "checkpoint agent must be an object",
            )
        })?;

    let compiled_prompt = required_string(definition, "compiled_prompt")?;
    if !agent.has_dynamic_instructions() {
        let instructions = agent.instructions().trim();
        if !instructions.is_empty()
            && compiled_prompt.trim() != instructions
            && !compiled_prompt.trim_start().starts_with(instructions)
        {
            return Err(CheckpointError::new(
                "checkpoint_definition_mismatch",
                "static agent instructions do not match the frozen checkpoint prompt",
            ));
        }
    }

    let stored_tool_names = definition
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_definition_invalid",
                "checkpoint tools must be an array",
            )
        })?
        .iter()
        .filter_map(|tool| {
            tool.pointer("/schema/function/name")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    let initial_messages = definition
        .get("initial_messages")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_definition_invalid",
                "checkpoint initial_messages must be an array",
            )
        })?
        .iter()
        .map(Message::from_dict)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| CheckpointError::new("checkpoint_definition_invalid", error))?;
    let initial_shared_state = definition
        .get("initial_shared_state")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_definition_invalid",
                "checkpoint initial_shared_state must be an object",
            )
        })?
        .clone()
        .into_iter()
        .collect();
    let mut metadata = checkpoint
        .messages
        .first()
        .filter(|message| message.role == MessageRole::System)
        .map(|message| message.metadata.clone())
        .unwrap_or_default();
    if let Some(run_metadata) = definition.get("run_metadata").and_then(Value::as_object) {
        metadata.extend(run_metadata.clone());
    }
    let tool_use_behavior = required_string(controls, "tool_use_behavior")?;
    metadata.insert(
        "_vv_agent_tool_use_behavior".to_string(),
        Value::String(tool_use_behavior.to_string()),
    );
    let stop_names = controls
        .get("stop_at_tool_names")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !stop_names.is_empty() {
        metadata.insert(
            "_vv_agent_stop_at_tool_names".to_string(),
            Value::Array(stop_names),
        );
    }

    let mut task = AgentTask::new(
        checkpoint.task_id.clone(),
        required_string(model, "model_id")?,
        compiled_prompt,
        required_string(definition, "root_input")?,
    );
    task.max_cycles = required_u32(controls, "max_cycles")?;
    task.memory_compact_threshold = required_u64(controls, "memory_compact_threshold")?;
    task.memory_threshold_percentage =
        u8::try_from(required_u64(controls, "memory_threshold_percentage")?).map_err(|_| {
            CheckpointError::new(
                "checkpoint_definition_invalid",
                "memory_threshold_percentage is outside the u8 range",
            )
        })?;
    task.no_tool_policy = match required_string(controls, "no_tool_policy")? {
        "continue" => NoToolPolicy::Continue,
        "wait_user" => NoToolPolicy::WaitUser,
        "finish" => NoToolPolicy::Finish,
        _ => {
            return Err(CheckpointError::new(
                "checkpoint_definition_invalid",
                "checkpoint no_tool_policy is invalid",
            ))
        }
    };
    task.allow_interruption = required_bool(controls, "allow_interruption")?;
    task.use_workspace = stored_tool_names
        .iter()
        .any(|name| WORKSPACE_TOOLS.contains(&name.as_str()));
    task.has_sub_agents = stored_tool_names.iter().any(|name| {
        matches!(
            name.as_str(),
            CREATE_SUB_TASK_TOOL_NAME | SUB_TASK_STATUS_TOOL_NAME
        )
    });
    task.sub_agents = agent.sub_agents().clone();
    task.agent_type = agent_definition
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_string);
    task.native_multimodal = required_bool(controls, "native_multimodal")?;
    task.extra_tool_names = stored_tool_names;
    task.initial_messages = initial_messages;
    task.initial_shared_state = initial_shared_state;
    task.model_settings = Some(model_settings.clone());
    task.metadata = metadata;
    Ok(task)
}

pub(crate) fn frozen_definition_messages(
    checkpoint: &CheckpointV2,
) -> CheckpointResult<Vec<Message>> {
    checkpoint
        .run_definition
        .get("initial_messages")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            CheckpointError::new(
                "checkpoint_definition_invalid",
                "checkpoint initial_messages must be an array",
            )
        })?
        .iter()
        .map(Message::from_dict)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| CheckpointError::new("checkpoint_definition_invalid", error))
}

pub(crate) fn validate_distributed_run_definition(
    envelope: &DistributedRunEnvelope,
    checkpoint: &CheckpointV2,
    resolved: Option<&ResolvedDistributedCapabilities>,
) -> CheckpointResult<()> {
    let comparison_definition = run_definition_comparison_copy(&checkpoint.run_definition);
    let definition = comparison_definition.as_object().ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            "checkpoint run definition must be an object",
        )
    })?;
    let controls = definition
        .get("runtime_controls")
        .and_then(Value::as_object)
        .ok_or_else(|| definition_invalid("runtime_controls must be an object"))?;
    let model = definition
        .get("model")
        .and_then(Value::as_object)
        .ok_or_else(|| definition_invalid("model must be an object"))?;
    let task = &envelope.task;

    let task_matches = definition.get("compiled_prompt").and_then(Value::as_str)
        == Some(task.system_prompt.as_str())
        && definition.get("root_input").and_then(Value::as_str) == Some(task.user_prompt.as_str())
        && model.get("model_id").and_then(Value::as_str) == Some(task.model.as_str())
        && controls.get("max_cycles").and_then(Value::as_u64) == Some(u64::from(task.max_cycles))
        && controls.get("no_tool_policy").and_then(Value::as_str)
            == Some(no_tool_policy_name(task.no_tool_policy))
        && controls
            .get("memory_compact_threshold")
            .and_then(Value::as_u64)
            == Some(task.memory_compact_threshold)
        && controls
            .get("memory_threshold_percentage")
            .and_then(Value::as_u64)
            == Some(u64::from(task.memory_threshold_percentage))
        && controls.get("allow_interruption").and_then(Value::as_bool)
            == Some(task.allow_interruption)
        && controls.get("native_multimodal").and_then(Value::as_bool)
            == Some(task.native_multimodal)
        && controls.get("tool_use_behavior").and_then(Value::as_str)
            == Some(
                task.metadata
                    .get("_vv_agent_tool_use_behavior")
                    .and_then(Value::as_str)
                    .unwrap_or("run_llm_again"),
            )
        && controls
            .get("stop_at_tool_names")
            .cloned()
            .unwrap_or_else(|| json!([]))
            == task
                .metadata
                .get("_vv_agent_stop_at_tool_names")
                .cloned()
                .unwrap_or_else(|| json!([]));
    if !task_matches {
        return Err(definition_mismatch(
            "distributed task does not match the embedded run definition",
        ));
    }
    if definition.get("initial_shared_state")
        != Some(&Value::Object(
            task.initial_shared_state.clone().into_iter().collect(),
        ))
        || definition.get("initial_messages")
            != Some(&Value::Array(
                task.initial_messages.iter().map(Message::to_dict).collect(),
            ))
    {
        return Err(definition_mismatch(
            "distributed initial state does not match the embedded run definition",
        ));
    }
    if model.get("backend").and_then(Value::as_str) != Some(envelope.recipe.backend.as_str())
        || model.get("model_id").and_then(Value::as_str) != Some(envelope.recipe.model.as_str())
    {
        return Err(definition_mismatch(
            "distributed recipe model does not match the embedded run definition",
        ));
    }

    let durable_slots = definition
        .get("credential_slots")
        .and_then(Value::as_array)
        .ok_or_else(|| definition_invalid("credential_slots must be an array"))?
        .iter()
        .map(|slot| {
            slot.as_str()
                .map(str::to_string)
                .ok_or_else(|| definition_invalid("credential_slots must contain strings"))
        })
        .collect::<CheckpointResult<Vec<_>>>()?;
    let settings = task.model_settings.clone().unwrap_or_default();
    let (settings_value, transport_timeout_seconds) = model_settings_definition(&settings)?;
    let mut candidate_definition = Value::Object(definition.clone());
    let candidate_model = candidate_definition
        .get_mut("model")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| definition_invalid("model must be an object"))?;
    candidate_model.insert("settings".to_string(), settings_value);
    if let Some(timeout) = transport_timeout_seconds {
        candidate_model.insert(
            "transport_timeout_seconds".to_string(),
            Value::from(timeout),
        );
    }
    let candidate_definition = normalize_run_definition(&candidate_definition, &durable_slots)?;
    if candidate_definition.get("model") != definition.get("model") {
        return Err(definition_mismatch(
            "distributed model settings do not match the embedded run definition",
        ));
    }
    let actual_budget = envelope
        .budget_limits
        .as_ref()
        .filter(|limits| limits.has_limits())
        .map(serde_json::to_value)
        .transpose()
        .map_err(|error| definition_invalid(error.to_string()))?
        .unwrap_or(Value::Null);
    if definition.get("budget_limits").unwrap_or(&Value::Null) != &actual_budget {
        return Err(definition_mismatch(
            "distributed budget limits do not match the embedded run definition",
        ));
    }
    let config = envelope
        .checkpoint_config
        .as_ref()
        .ok_or_else(|| definition_invalid("distributed checkpoint_config is missing"))?;
    let checkpoint_policy = definition
        .get("checkpoint_policy")
        .and_then(Value::as_object)
        .ok_or_else(|| definition_invalid("checkpoint_policy must be an object"))?;
    if checkpoint_policy.get("ambiguous_model_policy")
        != Some(&serde_json::to_value(config.ambiguous_model_policy).unwrap())
        || checkpoint_policy.get("ambiguous_tool_policy")
            != Some(&serde_json::to_value(config.ambiguous_tool_policy).unwrap())
        || checkpoint_policy
            .get("max_extension_state_bytes")
            .and_then(Value::as_u64)
            != Some(config.max_extension_state_bytes)
    {
        return Err(definition_mismatch(
            "distributed checkpoint policy does not match the embedded run definition",
        ));
    }
    if config
        .credential_slots
        .iter()
        .any(|slot| !durable_slots.iter().any(|candidate| candidate == slot))
    {
        return Err(definition_mismatch(
            "distributed credential slots do not match the embedded run definition",
        ));
    }

    let expected_policy = definition
        .get("tool_policy")
        .ok_or_else(|| definition_invalid("tool_policy is missing"))?;
    let actual_policy = json!({
        "allowed_tools": normalized_name_set(
            envelope.recipe.capabilities.tool_policy.allowed_tools.as_deref(),
        ),
        "disallowed_tools": normalized_name_set(Some(
            envelope.recipe.capabilities.tool_policy.disallowed_tools.as_slice(),
        ))
        .unwrap_or_default(),
        "approval": envelope.recipe.capabilities.tool_policy.approval,
        "predicate_ref": envelope.recipe.capabilities.tool_policy.predicate_ref,
        "approval_timeout_seconds": envelope.recipe.capabilities.approval_timeout_seconds,
        "denied_side_effects": envelope.recipe.capabilities.tool_policy.denied_side_effects,
        "denied_capability_tags": envelope.recipe.capabilities.tool_policy.denied_capability_tags,
        "deny_terminal_tools": envelope.recipe.capabilities.tool_policy.deny_terminal_tools,
        "denied_cost_dimensions": envelope.recipe.capabilities.tool_policy.denied_cost_dimensions,
    });
    if expected_policy != &actual_policy {
        return Err(definition_mismatch(
            "distributed tool policy does not match the embedded run definition",
        ));
    }

    let Some(resolved) = resolved else {
        return Ok(());
    };
    let mut refs = definition
        .get("capability_refs")
        .and_then(Value::as_object)
        .ok_or_else(|| definition_invalid("capability_refs must be an object"))?
        .iter()
        .map(|(slot, value)| {
            serde_json::from_value::<CapabilityRef>(value.clone())
                .map(|reference| (slot.clone(), reference))
                .map_err(|_| definition_invalid("capability_refs contains an invalid reference"))
        })
        .collect::<CheckpointResult<BTreeMap<_, _>>>()?;
    let actual_tools = tool_definitions(&resolved.tool_registry, task, &mut refs)?;
    if definition.get("tools") != Some(&Value::Array(actual_tools)) {
        return Err(definition_mismatch(
            "distributed tool schemas do not match the embedded run definition",
        ));
    }
    validate_definition_reference(
        definition.get("context_ref"),
        envelope.recipe.capabilities.app_state_ref.as_ref(),
        "context",
    )?;
    validate_definition_reference(
        definition.get("workspace_ref"),
        envelope.recipe.capabilities.workspace_backend_ref.as_ref(),
        "workspace",
    )?;
    validate_definition_reference(
        expected_policy.get("predicate_ref"),
        envelope
            .recipe
            .capabilities
            .tool_policy
            .predicate_ref
            .as_ref(),
        "tool_policy.predicate",
    )?;
    for (slot, actual) in [
        (
            "approval_provider",
            envelope.recipe.capabilities.approval_provider_ref.as_ref(),
        ),
        (
            "host_cost_meter",
            envelope.recipe.capabilities.host_cost_meter_ref.as_ref(),
        ),
        (
            "reconciliation_provider",
            envelope
                .recipe
                .capabilities
                .reconciliation_provider_ref
                .as_ref(),
        ),
        (
            "sub_task_manager",
            envelope.recipe.capabilities.sub_task_manager_ref.as_ref(),
        ),
    ] {
        if actual.is_some() || refs.contains_key(slot) {
            let expected = refs.remove(slot);
            if expected.as_ref() != actual {
                return Err(definition_mismatch(format!(
                    "distributed capability {slot:?} does not match the run definition"
                )));
            }
        }
    }
    for (prefix, actual) in [
        (
            "memory_provider",
            envelope.recipe.capabilities.memory_provider_refs.as_slice(),
        ),
        (
            "runtime_hook",
            envelope.recipe.capabilities.hook_refs.as_slice(),
        ),
        (
            "after_cycle_hook",
            envelope
                .recipe
                .capabilities
                .after_cycle_hook_refs
                .as_slice(),
        ),
    ] {
        for (index, reference) in actual.iter().enumerate() {
            let slot = format!("{prefix}:{index}");
            if refs.remove(&slot).as_ref() != Some(reference) {
                return Err(definition_mismatch(format!(
                    "distributed capability {slot:?} does not match the run definition"
                )));
            }
        }
        if refs
            .keys()
            .any(|slot| slot.starts_with(&format!("{prefix}:")))
        {
            return Err(definition_mismatch(format!(
                "distributed capabilities are missing run-definition {prefix} refs"
            )));
        }
    }

    let expected_extensions = definition
        .get("extensions")
        .and_then(Value::as_array)
        .ok_or_else(|| definition_invalid("extensions must be an array"))?;
    if expected_extensions.len() != resolved.checkpoint_extensions.len()
        || resolved.checkpoint_extensions.iter().any(|resolved| {
            !expected_extensions.iter().any(|expected| {
                expected.get("namespace").and_then(Value::as_str)
                    == Some(resolved.descriptor.namespace.as_str())
                    && expected.get("version").and_then(Value::as_str)
                        == Some(resolved.extension.version())
                    && expected.get("required").and_then(Value::as_bool)
                        == Some(resolved.descriptor.required)
            })
        })
    {
        return Err(definition_mismatch(
            "distributed checkpoint extensions do not match the run definition",
        ));
    }
    Ok(())
}

fn validate_definition_reference(
    expected: Option<&Value>,
    actual: Option<&CapabilityRef>,
    slot: &str,
) -> CheckpointResult<()> {
    let expected = expected.filter(|value| !value.is_null());
    let actual = actual.map(|reference| serde_json::to_value(reference).unwrap());
    if expected != actual.as_ref() {
        return Err(definition_mismatch(format!(
            "distributed capability {slot:?} does not match the run definition"
        )));
    }
    Ok(())
}

fn definition_invalid(message: impl Into<String>) -> CheckpointError {
    CheckpointError::new("checkpoint_definition_invalid", message)
}

fn definition_mismatch(message: impl Into<String>) -> CheckpointError {
    CheckpointError::new("checkpoint_definition_mismatch", message)
}

fn required_string<'a>(object: &'a Map<String, Value>, field: &str) -> CheckpointResult<&'a str> {
    object.get(field).and_then(Value::as_str).ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            format!("checkpoint run definition field {field:?} must be a string"),
        )
    })
}

fn required_u64(object: &Map<String, Value>, field: &str) -> CheckpointResult<u64> {
    object.get(field).and_then(Value::as_u64).ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            format!("checkpoint run definition field {field:?} must be an unsigned integer"),
        )
    })
}

fn required_u32(object: &Map<String, Value>, field: &str) -> CheckpointResult<u32> {
    u32::try_from(required_u64(object, field)?).map_err(|_| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            format!("checkpoint run definition field {field:?} exceeds u32"),
        )
    })
}

fn required_bool(object: &Map<String, Value>, field: &str) -> CheckpointResult<bool> {
    object.get(field).and_then(Value::as_bool).ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            format!("checkpoint run definition field {field:?} must be a boolean"),
        )
    })
}

fn model_settings_definition(settings: &ModelSettings) -> CheckpointResult<(Value, Option<f64>)> {
    let mut value = settings.to_value();
    let object = value.as_object_mut().ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_definition_invalid",
            "model settings must serialize as an object",
        )
    })?;
    let timeout = object
        .remove("timeout_seconds")
        .and_then(|value| value.as_f64());

    if let Some(headers) = object
        .get_mut("extra_headers")
        .and_then(Value::as_object_mut)
    {
        let original = std::mem::take(headers);
        let mut normalized = Map::new();
        for (name, value) in original {
            if !name.is_ascii() {
                return Err(CheckpointError::new(
                    "checkpoint_definition_invalid",
                    "model extra header names must be ASCII strings",
                ));
            }
            let name = name.to_ascii_lowercase();
            if normalized.insert(name.clone(), value).is_some() {
                return Err(CheckpointError::new(
                    "checkpoint_definition_header_collision",
                    format!("model extra header name collides after lowercasing: {name}"),
                ));
            }
        }
        *headers = normalized;
    }
    Ok((value, timeout))
}

fn tool_definitions(
    registry: &ToolRegistry,
    task: &AgentTask,
    refs: &mut BTreeMap<String, CapabilityRef>,
) -> CheckpointResult<Vec<Value>> {
    let mut definitions = Vec::new();
    for schema in plan_tool_schemas(registry, task, None) {
        let Some(name) = schema
            .get("function")
            .and_then(Value::as_object)
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Ok(spec) = registry.get(name) else {
            continue;
        };
        let approval = match &spec.approval {
            ToolApprovalRule::Static(ApprovalRequirement::NotRequired) => {
                json!({"mode": "static", "required": false})
            }
            ToolApprovalRule::Static(ApprovalRequirement::Required) => {
                json!({"mode": "static", "required": true})
            }
            ToolApprovalRule::Static(ApprovalRequirement::Provider)
            | ToolApprovalRule::Predicate(_) => {
                let reference = take_ref(refs, &format!("tool_approval:{name}"), true)?;
                json!({"mode": "referenced", "ref": reference})
            }
        };
        definitions.push(json!({
            "schema": schema,
            "idempotency": spec.idempotency,
            "tool_metadata": spec.tool_metadata.as_ref(),
            "timeout_seconds": spec.timeout.map(|timeout| timeout.as_secs_f64()),
            "approval": approval,
        }));
    }
    Ok(definitions)
}

fn extension_definitions(run_config: &RunConfig) -> CheckpointResult<Vec<Value>> {
    let config = run_config.checkpoint_config.as_ref().ok_or_else(|| {
        CheckpointError::new(
            "checkpoint_config_invalid",
            "checkpoint_config is required to define extensions",
        )
    })?;
    let required = config
        .required_extension_namespaces
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    let mut definitions = Vec::new();
    for extension in &run_config.checkpoint_extensions {
        let namespace = extension.namespace();
        validate_extension_namespace(namespace)?;
        if extension.version().trim().is_empty() {
            return Err(CheckpointError::new(
                "checkpoint_extension_version_invalid",
                format!("checkpoint extension {namespace} has an empty version"),
            ));
        }
        if !seen.insert(namespace.to_string()) {
            return Err(CheckpointError::new(
                "checkpoint_extension_namespace_duplicate",
                format!("duplicate checkpoint extension {namespace}"),
            ));
        }
        definitions.push(json!({
            "namespace": namespace,
            "version": extension.version(),
            "required": extension.required() || required.contains(namespace),
        }));
    }
    if let Some(missing) = required.iter().find(|namespace| !seen.contains(*namespace)) {
        return Err(CheckpointError::new(
            "checkpoint_extension_missing",
            format!("required checkpoint extension {missing} is unavailable"),
        ));
    }
    definitions.sort_by(|left, right| {
        utf16_cmp(
            left["namespace"].as_str().unwrap_or_default(),
            right["namespace"].as_str().unwrap_or_default(),
        )
    });
    Ok(definitions)
}

fn validate_behavior_capability_refs(
    agent: &Agent,
    run_config: &RunConfig,
    refs: &BTreeMap<String, CapabilityRef>,
) -> CheckpointResult<()> {
    let mut required = Vec::new();
    if agent.has_dynamic_instructions() {
        required.push("agent.instructions".to_string());
    }
    required.extend(
        agent
            .input_guardrails()
            .iter()
            .enumerate()
            .map(|(index, _)| format!("input_guardrail:{index}")),
    );
    required.extend(
        agent
            .output_guardrails()
            .iter()
            .enumerate()
            .map(|(index, _)| format!("output_guardrail:{index}")),
    );
    required.extend(
        agent
            .hooks()
            .iter()
            .chain(run_config.hooks.iter())
            .enumerate()
            .map(|(index, _)| format!("runtime_hook:{index}")),
    );
    required.extend(
        run_config
            .after_cycle_hooks
            .iter()
            .enumerate()
            .map(|(index, _)| format!("after_cycle_hook:{index}")),
    );
    required.extend(
        run_config
            .context_providers
            .iter()
            .enumerate()
            .map(|(index, _)| format!("context_provider:{index}")),
    );
    required.extend(
        run_config
            .memory_providers
            .iter()
            .enumerate()
            .map(|(index, _)| format!("memory_provider:{index}")),
    );
    for (present, slot) in [
        (
            !behavior_metadata(agent, run_config)
                .as_object()
                .is_none_or(Map::is_empty),
            "behavior_affecting_run_metadata",
        ),
        (
            run_config.before_cycle_messages.is_some(),
            "before_cycle_messages",
        ),
        (
            run_config.interruption_messages.is_some(),
            "interruption_messages",
        ),
        (run_config.approval_provider.is_some(), "approval_provider"),
        (run_config.host_cost_meter.is_some(), "host_cost_meter"),
        (
            run_config.reconciliation_provider.is_some(),
            "reconciliation_provider",
        ),
        (
            run_config.tool_registry_factory.is_some(),
            "tool_registry_factory",
        ),
        (run_config.sub_task_manager.is_some(), "sub_task_manager"),
        (agent.output_type_name().is_some(), "output_validator"),
    ] {
        if present {
            required.push(slot.to_string());
        }
    }
    let missing = required
        .into_iter()
        .filter(|slot| !refs.contains_key(slot))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(CheckpointError::new(
            "checkpoint_definition_unstable",
            format!(
                "checkpoint v2 requires stable capability refs for: {}",
                missing.join(", ")
            ),
        ));
    }
    Ok(())
}

fn take_ref(
    refs: &mut BTreeMap<String, CapabilityRef>,
    slot: &str,
    required: bool,
) -> CheckpointResult<Option<CapabilityRef>> {
    let value = refs.remove(slot);
    if required && value.is_none() {
        return Err(CheckpointError::new(
            "checkpoint_definition_unstable",
            format!("checkpoint v2 requires stable capability ref {slot}"),
        ));
    }
    Ok(value)
}
