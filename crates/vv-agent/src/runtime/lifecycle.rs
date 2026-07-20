use std::cmp::Ordering;
use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::types::{CompletionReason, CycleRecord, Message, Metadata, TaskTokenUsage};

pub const AFTER_CYCLE_CONTROL_STATE_KEY: &str = "_vv_agent_after_cycle_control";
pub const AFTER_CYCLE_CONTROL_SCHEMA: &str = "vv-agent.after-cycle-control.v1";
pub const MAX_STEERING_MESSAGES: usize = 32;
pub const MAX_STEERING_MESSAGE_UTF8_BYTES: usize = 16_384;
pub const MAX_TOTAL_STEERING_UTF8_BYTES: usize = 65_536;
pub const MAX_DISALLOW_TOOLS: usize = 1_024;
pub const MAX_TOOL_NAME_UTF8_BYTES: usize = 256;
pub const MAX_STOP_CODE_ASCII_BYTES: usize = 128;
pub const MAX_STOP_MESSAGE_UTF8_BYTES: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AfterCycleAction {
    Continue,
    Steer,
    StopNonSuccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeCycleOutcomeKind {
    Continue,
    Completed,
    WaitUser,
    MaxCycles,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeCycleOutcome {
    pub kind: NativeCycleOutcomeKind,
    pub completion_reason: Option<CompletionReason>,
    pub completion_tool_name: Option<String>,
    pub steer_allowed: bool,
}

impl NativeCycleOutcome {
    pub fn continuing() -> Self {
        Self {
            kind: NativeCycleOutcomeKind::Continue,
            completion_reason: None,
            completion_tool_name: None,
            steer_allowed: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AfterCycleSnapshot {
    pub task_id: String,
    pub cycle_index: u32,
    pub max_cycles: u32,
    pub remaining_cycles: u32,
    pub cycle: CycleRecord,
    pub messages: Vec<Message>,
    pub shared_state: Metadata,
    pub cumulative_token_usage: TaskTokenUsage,
    pub available_tool_names: Vec<String>,
    pub disallowed_tool_names: Vec<String>,
    pub native_outcome: NativeCycleOutcome,
}

impl AfterCycleSnapshot {
    #[allow(clippy::too_many_arguments)]
    pub fn capture(
        task_id: impl Into<String>,
        cycle_index: u32,
        max_cycles: u32,
        cycle: &CycleRecord,
        messages: &[Message],
        shared_state: &Metadata,
        cumulative_token_usage: TaskTokenUsage,
        available_tool_names: Vec<String>,
        disallowed_tool_names: Vec<String>,
        native_outcome: NativeCycleOutcome,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            cycle_index,
            max_cycles,
            remaining_cycles: max_cycles.saturating_sub(cycle_index),
            cycle: cycle.clone(),
            messages: messages.to_vec(),
            shared_state: shared_state.clone(),
            cumulative_token_usage,
            available_tool_names,
            disallowed_tool_names,
            native_outcome,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AfterCycleStop {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AfterCycleDecision {
    pub action: AfterCycleAction,
    pub steering_messages: Vec<String>,
    pub disallow_tools: Vec<String>,
    pub stop: Option<AfterCycleStop>,
}

impl Default for AfterCycleDecision {
    fn default() -> Self {
        Self::continue_run()
    }
}

impl AfterCycleDecision {
    pub fn continue_run() -> Self {
        Self {
            action: AfterCycleAction::Continue,
            steering_messages: Vec::new(),
            disallow_tools: Vec::new(),
            stop: None,
        }
    }

    pub fn continue_with_disallowed_tools(
        tools: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, AfterCycleDecisionError> {
        let decision = Self {
            disallow_tools: tools.into_iter().map(Into::into).collect(),
            ..Self::continue_run()
        };
        decision.validate()?;
        Ok(decision)
    }

    pub fn steer(
        messages: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, AfterCycleDecisionError> {
        Self::steer_with_disallowed_tools(messages, Vec::<String>::new())
    }

    pub fn steer_with_disallowed_tools(
        messages: impl IntoIterator<Item = impl Into<String>>,
        tools: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, AfterCycleDecisionError> {
        let decision = Self {
            action: AfterCycleAction::Steer,
            steering_messages: messages.into_iter().map(Into::into).collect(),
            disallow_tools: tools.into_iter().map(Into::into).collect(),
            stop: None,
        };
        decision.validate()?;
        Ok(decision)
    }

    pub fn stop_non_success(
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<Self, AfterCycleDecisionError> {
        let decision = Self {
            action: AfterCycleAction::StopNonSuccess,
            steering_messages: Vec::new(),
            disallow_tools: Vec::new(),
            stop: Some(AfterCycleStop {
                code: code.into(),
                message: message.into(),
            }),
        };
        decision.validate()?;
        Ok(decision)
    }

    pub fn validate(&self) -> Result<(), AfterCycleDecisionError> {
        if self.steering_messages.len() > MAX_STEERING_MESSAGES {
            return Err(AfterCycleDecisionError::new(
                "after-cycle steering message count exceeds the limit",
            ));
        }
        let mut total_bytes = 0_usize;
        for message in &self.steering_messages {
            validate_bounded_text(
                message,
                "after-cycle steering message",
                MAX_STEERING_MESSAGE_UTF8_BYTES,
            )?;
            total_bytes = total_bytes.saturating_add(message.len());
        }
        if total_bytes > MAX_TOTAL_STEERING_UTF8_BYTES {
            return Err(AfterCycleDecisionError::new(
                "after-cycle steering messages exceed the total byte limit",
            ));
        }
        if self.disallow_tools.len() > MAX_DISALLOW_TOOLS {
            return Err(AfterCycleDecisionError::new(
                "after-cycle disallowed tool count exceeds the limit",
            ));
        }
        let mut seen = HashSet::new();
        for tool_name in &self.disallow_tools {
            validate_bounded_text(
                tool_name,
                "after-cycle disallowed tool name",
                MAX_TOOL_NAME_UTF8_BYTES,
            )?;
            if !seen.insert(tool_name) {
                return Err(AfterCycleDecisionError::new(
                    "after-cycle disallowed tools must be unique",
                ));
            }
        }
        match self.action {
            AfterCycleAction::Continue => {
                if !self.steering_messages.is_empty() || self.stop.is_some() {
                    return Err(AfterCycleDecisionError::new(
                        "continue cannot include steering messages or a stop payload",
                    ));
                }
            }
            AfterCycleAction::Steer => {
                if self.steering_messages.is_empty() || self.stop.is_some() {
                    return Err(AfterCycleDecisionError::new(
                        "steer requires messages and cannot include a stop payload",
                    ));
                }
            }
            AfterCycleAction::StopNonSuccess => {
                if !self.steering_messages.is_empty()
                    || !self.disallow_tools.is_empty()
                    || self.stop.is_none()
                {
                    return Err(AfterCycleDecisionError::new(
                        "stop_non_success requires only a typed stop payload",
                    ));
                }
                let stop = self.stop.as_ref().expect("checked stop payload");
                validate_stop_code(&stop.code)?;
                validate_bounded_text(
                    &stop.message,
                    "after-cycle stop message",
                    MAX_STOP_MESSAGE_UTF8_BYTES,
                )?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AfterCycleDecisionError {
    message: String,
}

impl AfterCycleDecisionError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for AfterCycleDecisionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AfterCycleDecisionError {}

pub trait AfterCycleHook: Send + Sync {
    fn after_cycle(
        &self,
        snapshot: &AfterCycleSnapshot,
    ) -> Result<Option<AfterCycleDecision>, String>;
}

impl<F> AfterCycleHook for F
where
    F: Fn(&AfterCycleSnapshot) -> Result<Option<AfterCycleDecision>, String> + Send + Sync,
{
    fn after_cycle(
        &self,
        snapshot: &AfterCycleSnapshot,
    ) -> Result<Option<AfterCycleDecision>, String> {
        self(snapshot)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AfterCycleHookError {
    pub code: &'static str,
    message: String,
}

impl AfterCycleHookError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for AfterCycleHookError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AfterCycleHookError {}

#[derive(Clone, Default)]
pub struct AfterCycleHookManager {
    hooks: Vec<Arc<dyn AfterCycleHook>>,
}

impl AfterCycleHookManager {
    pub fn new(hooks: Vec<Arc<dyn AfterCycleHook>>) -> Self {
        Self { hooks }
    }

    pub fn has_hooks(&self) -> bool {
        !self.hooks.is_empty()
    }

    pub fn apply(
        &self,
        snapshot: &AfterCycleSnapshot,
    ) -> Result<AfterCycleDecision, AfterCycleHookError> {
        let mut steering_messages = Vec::new();
        let mut disallow_tools = Vec::new();
        let mut seen_tools = HashSet::new();
        for hook in &self.hooks {
            let outcome = catch_unwind(AssertUnwindSafe(|| hook.after_cycle(snapshot)))
                .map_err(|_| {
                    AfterCycleHookError::new("after_cycle_hook_failed", "after-cycle hook panicked")
                })?
                .map_err(|error| {
                    AfterCycleHookError::new(
                        "after_cycle_hook_failed",
                        format!("after-cycle hook failed: {error}"),
                    )
                })?;
            let Some(decision) = outcome else {
                continue;
            };
            decision.validate().map_err(|error| {
                AfterCycleHookError::new(
                    "after_cycle_decision_invalid",
                    format!("after-cycle hook returned an invalid decision: {error}"),
                )
            })?;
            for tool_name in &decision.disallow_tools {
                if seen_tools.insert(tool_name.clone()) {
                    disallow_tools.push(tool_name.clone());
                }
            }
            if decision.action == AfterCycleAction::StopNonSuccess {
                return Ok(decision);
            }
            if decision.action == AfterCycleAction::Steer {
                steering_messages.extend(decision.steering_messages);
            }
        }
        let composed = if steering_messages.is_empty() {
            AfterCycleDecision::continue_with_disallowed_tools(disallow_tools)
        } else {
            AfterCycleDecision::steer_with_disallowed_tools(steering_messages, disallow_tools)
        };
        composed.map_err(|error| {
            AfterCycleHookError::new(
                "after_cycle_decision_invalid",
                format!("composed after-cycle decision is invalid: {error}"),
            )
        })
    }
}

pub fn read_after_cycle_disallowed_tools(
    shared_state: &Metadata,
) -> Result<Vec<String>, AfterCycleHookError> {
    let Some(raw) = shared_state.get(AFTER_CYCLE_CONTROL_STATE_KEY) else {
        return Ok(Vec::new());
    };
    let object = raw
        .as_object()
        .ok_or_else(|| control_state_error("after-cycle control state must be an object"))?;
    let expected = BTreeSet::from(["schema_version", "disallowed_tools"]);
    let actual = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(control_state_error(
            "after-cycle control state has missing or unknown fields",
        ));
    }
    if object.get("schema_version").and_then(Value::as_str) != Some(AFTER_CYCLE_CONTROL_SCHEMA) {
        return Err(control_state_error(
            "after-cycle control state schema is unsupported",
        ));
    }
    let values = object
        .get("disallowed_tools")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            control_state_error("after-cycle control disallowed_tools must be an array")
        })?
        .iter()
        .map(|value| {
            value.as_str().map(str::to_string).ok_or_else(|| {
                control_state_error("after-cycle control disallowed_tools must contain strings")
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    AfterCycleDecision::continue_with_disallowed_tools(values.clone())
        .map_err(|error| control_state_error(error.to_string()))?;
    let mut ordered = values.clone();
    ordered.sort_by(|left, right| utf16_cmp(left, right));
    if values != ordered {
        return Err(control_state_error(
            "after-cycle control disallowed_tools must be sorted and unique",
        ));
    }
    Ok(ordered)
}

pub fn persist_after_cycle_disallowed_tools(
    shared_state: &mut Metadata,
    additional_tools: &[String],
) -> Result<Vec<String>, AfterCycleHookError> {
    let mut values = read_after_cycle_disallowed_tools(shared_state)?;
    values.extend(additional_tools.iter().cloned());
    values.sort_by(|left, right| utf16_cmp(left, right));
    values.dedup();
    if values.is_empty() {
        return Ok(values);
    }
    AfterCycleDecision::continue_with_disallowed_tools(values.clone())
        .map_err(|error| control_state_error(error.to_string()))?;
    shared_state.insert(
        AFTER_CYCLE_CONTROL_STATE_KEY.to_string(),
        json!({
            "schema_version": AFTER_CYCLE_CONTROL_SCHEMA,
            "disallowed_tools": values,
        }),
    );
    Ok(values)
}

pub(crate) fn utf16_cmp(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

fn validate_bounded_text(
    value: &str,
    field_name: &str,
    max_bytes: usize,
) -> Result<(), AfterCycleDecisionError> {
    if value.trim().is_empty() {
        return Err(AfterCycleDecisionError::new(format!(
            "{field_name} must be a non-empty string"
        )));
    }
    if value.len() > max_bytes {
        return Err(AfterCycleDecisionError::new(format!(
            "{field_name} exceeds {max_bytes} UTF-8 bytes"
        )));
    }
    Ok(())
}

fn validate_stop_code(code: &str) -> Result<(), AfterCycleDecisionError> {
    let valid = code.is_ascii()
        && !code.is_empty()
        && code.len() <= MAX_STOP_CODE_ASCII_BYTES
        && code.as_bytes()[0].is_ascii_lowercase()
        && code.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_.-".contains(&byte)
        });
    if !valid {
        return Err(AfterCycleDecisionError::new(
            "after-cycle stop code is invalid",
        ));
    }
    Ok(())
}

fn control_state_error(message: impl Into<String>) -> AfterCycleHookError {
    AfterCycleHookError::new("after_cycle_control_state_invalid", message)
}
