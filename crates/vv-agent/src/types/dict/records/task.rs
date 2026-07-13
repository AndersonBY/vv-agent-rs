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
            (
                "model_settings".to_string(),
                serde_json::to_value(&self.model_settings).unwrap_or(Value::Null),
            ),
            ("metadata".to_string(), metadata_to_value(&self.metadata)),
        ]))
    }

    pub fn from_dict(data: &Value) -> Result<Self, String> {
        serde_json::from_value(data.clone()).map_err(|error| error.to_string())
    }
}
