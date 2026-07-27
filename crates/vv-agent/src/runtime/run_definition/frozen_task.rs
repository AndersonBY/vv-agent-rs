use serde_json::Value;

use crate::agent::Agent;
use crate::checkpoint::{CheckpointError, CheckpointResult};
use crate::constants::WORKSPACE_TOOLS;
use crate::model_settings::ModelSettings;
use crate::prompt::PromptBundle;
use crate::runtime::state::Checkpoint;
use crate::types::{AgentTask, Message, MessageRole, NoToolPolicy};

use super::{
    definition_invalid, frozen_prompt, required_bool, required_string, required_u32, required_u64,
};

pub(crate) fn build_frozen_task(
    agent: &Agent,
    checkpoint: &Checkpoint,
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

    let prompt_bundle = definition
        .get("prompt_bundle")
        .ok_or_else(|| definition_invalid("prompt_bundle is missing"))
        .and_then(|value| {
            PromptBundle::from_value(value)
                .map_err(|error| definition_invalid(format!("prompt_bundle is invalid: {error}")))
        })?;
    frozen_prompt::validate_frozen_checkpoint_messages(&checkpoint.messages, &prompt_bundle)?;
    if !agent.has_dynamic_instructions() {
        let instructions = agent.instructions().trim();
        if !instructions.is_empty()
            && prompt_bundle.flatten().trim() != instructions
            && !prompt_bundle
                .flatten()
                .trim_start()
                .starts_with(instructions)
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
    metadata.insert(
        "session_memory_enabled".to_string(),
        Value::Bool(required_bool(controls, "session_memory_enabled")?),
    );
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
        prompt_bundle,
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
    task.microcompaction_policy = serde_json::from_value(
        controls
            .get("microcompaction_policy")
            .cloned()
            .ok_or_else(|| definition_invalid("microcompaction_policy is missing"))?,
    )
    .map_err(|error| definition_invalid(format!("microcompaction_policy is invalid: {error}")))?;
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
    checkpoint: &Checkpoint,
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
