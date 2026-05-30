use std::collections::BTreeMap;

use serde_json::Value;

use crate::llm::LlmClient;
use crate::types::Message;

use super::helpers::drain_steering_queue;
use super::{AgentRuntime, RuntimeRunControls};

impl<C: LlmClient> AgentRuntime<C> {
    pub(super) fn apply_cycle_inputs(
        &self,
        controls: &RuntimeRunControls,
        cycle_index: u32,
        messages: &mut Vec<Message>,
        shared_state: &BTreeMap<String, Value>,
    ) {
        let before_cycle_messages = controls
            .before_cycle_messages
            .as_ref()
            .map(|provider| provider(cycle_index, messages, shared_state))
            .unwrap_or_default();
        if !before_cycle_messages.is_empty() {
            let message_count = before_cycle_messages.len();
            messages.extend(before_cycle_messages);
            self.emit_log(
                controls,
                "cycle_injected_messages",
                BTreeMap::from([
                    ("cycle".to_string(), Value::from(cycle_index)),
                    (
                        "reason".to_string(),
                        Value::String("before_cycle_messages".to_string()),
                    ),
                    (
                        "message_count".to_string(),
                        Value::from(message_count as u64),
                    ),
                ]),
            );
        }

        let cycle_steering_prompts = drain_steering_queue(controls);
        if cycle_steering_prompts.is_empty() {
            return;
        }
        for prompt in &cycle_steering_prompts {
            messages.push(Message::user(prompt.clone()));
            self.emit_log(
                controls,
                "session_steer_dequeued",
                BTreeMap::from([
                    ("cycle".to_string(), Value::from(cycle_index)),
                    ("prompt".to_string(), Value::String(prompt.clone())),
                ]),
            );
        }
        self.emit_log(
            controls,
            "cycle_injected_messages",
            BTreeMap::from([
                ("cycle".to_string(), Value::from(cycle_index)),
                (
                    "reason".to_string(),
                    Value::String("session_steering".to_string()),
                ),
                (
                    "message_count".to_string(),
                    Value::from(cycle_steering_prompts.len() as u64),
                ),
            ]),
        );
    }
}
