use serde_json::Value;

use crate::types::{AgentTask, Message};

use super::super::common::*;

impl AgentTask {
    pub fn to_dict(&self) -> Value {
        Value::Object(serde_json::Map::from_iter([
            ("task_id".to_string(), Value::String(self.task_id.clone())),
            ("model".to_string(), Value::String(self.model.clone())),
            (
                "system_prompt".to_string(),
                Value::String(self.system_prompt.clone()),
            ),
            (
                "user_prompt".to_string(),
                Value::String(self.user_prompt.clone()),
            ),
            ("max_cycles".to_string(), Value::from(self.max_cycles)),
            (
                "memory_compact_threshold".to_string(),
                Value::from(self.memory_compact_threshold),
            ),
            (
                "memory_threshold_percentage".to_string(),
                Value::from(self.memory_threshold_percentage),
            ),
            (
                "no_tool_policy".to_string(),
                Value::String(no_tool_policy_value(self.no_tool_policy).to_string()),
            ),
            (
                "allow_interruption".to_string(),
                Value::Bool(self.allow_interruption),
            ),
            ("use_workspace".to_string(), Value::Bool(self.use_workspace)),
            (
                "has_sub_agents".to_string(),
                Value::Bool(self.has_sub_agents),
            ),
            (
                "sub_agents".to_string(),
                serde_json::to_value(&self.sub_agents).unwrap_or(Value::Null),
            ),
            (
                "agent_type".to_string(),
                self.agent_type
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            ),
            (
                "native_multimodal".to_string(),
                Value::Bool(self.native_multimodal),
            ),
            (
                "extra_tool_names".to_string(),
                string_vec_to_value(&self.extra_tool_names),
            ),
            (
                "exclude_tools".to_string(),
                string_vec_to_value(&self.exclude_tools),
            ),
            (
                "initial_messages".to_string(),
                Value::Array(self.initial_messages.iter().map(Message::to_dict).collect()),
            ),
            (
                "initial_shared_state".to_string(),
                metadata_to_value(&self.initial_shared_state),
            ),
            ("metadata".to_string(), metadata_to_value(&self.metadata)),
        ]))
    }

    pub fn from_dict(data: &Value) -> Result<Self, String> {
        let object = expect_object(data, "AgentTask")?;
        let mut task = Self::new(
            read_required_string(object, "task_id")?,
            read_required_string(object, "model")?,
            read_required_string(object, "system_prompt")?,
            read_required_string(object, "user_prompt")?,
        );
        task.max_cycles = read_u32(object, "max_cycles", 8);
        task.memory_compact_threshold = read_u64(object, "memory_compact_threshold", 128_000);
        task.memory_threshold_percentage = read_u8(object, "memory_threshold_percentage", 90);
        task.no_tool_policy = parse_no_tool_policy(
            read_optional_string(object, "no_tool_policy")
                .as_deref()
                .unwrap_or("continue"),
        )?;
        task.allow_interruption = read_bool(object, "allow_interruption", true);
        task.use_workspace = read_bool(object, "use_workspace", true);
        task.has_sub_agents = read_bool(object, "has_sub_agents", false);
        if let Some(value) = object.get("sub_agents") {
            task.sub_agents =
                serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
        }
        task.agent_type = read_optional_string(object, "agent_type");
        task.native_multimodal = read_bool(object, "native_multimodal", false);
        task.extra_tool_names = read_string_list(object, "extra_tool_names");
        task.exclude_tools = read_string_list(object, "exclude_tools");
        task.initial_messages = read_array(object, "initial_messages")
            .unwrap_or(&[])
            .iter()
            .map(Message::from_dict)
            .collect::<Result<Vec<_>, _>>()?;
        task.initial_shared_state = read_metadata(object, "initial_shared_state")?;
        task.metadata = read_metadata(object, "metadata")?;
        Ok(task)
    }
}
