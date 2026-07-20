use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::llm::LlmClient;
use crate::runtime::lifecycle::{
    persist_after_cycle_disallowed_tools, read_after_cycle_disallowed_tools, utf16_cmp,
    AfterCycleAction, AfterCycleDecision, AfterCycleHookManager, AfterCycleSnapshot,
    NativeCycleOutcome, NativeCycleOutcomeKind,
};
use crate::runtime::token_usage::summarize_task_token_usage;
use crate::types::{
    AgentResult, AgentTask, CompletionReason, CycleRecord, LLMResponse, Message, NoToolPolicy,
    ToolDirective, ToolExecutionResult,
};

use super::budget::RunBudgetController;
use super::checkpoint::CheckpointCoordinator;
use super::completion::{
    handle_directive_result, handle_no_tool_response, DirectiveResultRequest, NoToolResponseRequest,
};
use super::helpers::failed_agent_result;
use super::{AgentRuntime, RuntimeRunControls};

pub(super) struct NoToolCycleFinalization<'a, C: LlmClient> {
    pub(super) runtime: &'a AgentRuntime<C>,
    pub(super) controls: &'a RuntimeRunControls,
    pub(super) task: &'a AgentTask,
    pub(super) cycle_index: u32,
    pub(super) response: &'a LLMResponse,
    pub(super) cycle: CycleRecord,
    pub(super) messages: &'a mut Vec<Message>,
    pub(super) cycles: &'a mut Vec<CycleRecord>,
    pub(super) shared_state: &'a mut BTreeMap<String, Value>,
    pub(super) checkpoint: &'a CheckpointCoordinator,
    pub(super) budget_controller: &'a Option<RunBudgetController>,
    pub(super) persisted_denials: &'a [String],
}

pub(super) fn finalize_no_tool_cycle<C: LlmClient + Clone + 'static>(
    request: NoToolCycleFinalization<'_, C>,
) -> Option<AgentResult> {
    let NoToolCycleFinalization {
        runtime,
        controls,
        task,
        cycle_index,
        response,
        cycle,
        messages,
        cycles,
        shared_state,
        checkpoint,
        budget_controller,
        persisted_denials,
    } = request;
    cycles.push(cycle);
    let native_outcome = match task.no_tool_policy {
        NoToolPolicy::Finish => NativeCycleOutcome {
            kind: NativeCycleOutcomeKind::Completed,
            completion_reason: Some(CompletionReason::NoToolFinish),
            completion_tool_name: None,
            steer_allowed: cycle_index < task.max_cycles,
        },
        NoToolPolicy::WaitUser => NativeCycleOutcome {
            kind: NativeCycleOutcomeKind::WaitUser,
            completion_reason: Some(CompletionReason::WaitUser),
            completion_tool_name: None,
            steer_allowed: false,
        },
        NoToolPolicy::Continue if cycle_index >= task.max_cycles => NativeCycleOutcome {
            kind: NativeCycleOutcomeKind::MaxCycles,
            completion_reason: Some(CompletionReason::MaxCycles),
            completion_tool_name: None,
            steer_allowed: false,
        },
        NoToolPolicy::Continue => NativeCycleOutcome::continuing(),
    };
    let completed_cycle = cycles
        .last()
        .expect("no-tool cycle was appended before after-cycle hooks")
        .clone();
    let decision = match runtime.apply_after_cycle_hooks(
        controls,
        task,
        &completed_cycle,
        messages,
        cycles,
        shared_state,
        native_outcome,
        persisted_denials,
    ) {
        Ok(decision) => decision,
        Err(result) => return Some(*result),
    };
    if decision
        .as_ref()
        .is_some_and(|decision| decision.action == AfterCycleAction::Steer)
    {
        return commit_nonterminal_cycle(
            checkpoint,
            task,
            cycle_index,
            messages,
            cycles,
            shared_state,
            budget_controller,
        );
    }
    if let Some(result) = handle_no_tool_response(NoToolResponseRequest {
        runtime,
        controls,
        task,
        cycle_index,
        response,
        messages,
        cycles,
        shared_state,
    }) {
        return Some(result);
    }
    commit_nonterminal_cycle(
        checkpoint,
        task,
        cycle_index,
        messages,
        cycles,
        shared_state,
        budget_controller,
    )
}

pub(super) struct ToolCycleFinalization<'a, C: LlmClient> {
    pub(super) runtime: &'a AgentRuntime<C>,
    pub(super) controls: &'a RuntimeRunControls,
    pub(super) task: &'a AgentTask,
    pub(super) cycle_index: u32,
    pub(super) directive_result: Option<&'a ToolExecutionResult>,
    pub(super) completion_reason: Option<CompletionReason>,
    pub(super) completion_tool_name: Option<&'a str>,
    pub(super) messages: &'a mut Vec<Message>,
    pub(super) cycles: &'a mut Vec<CycleRecord>,
    pub(super) shared_state: &'a mut BTreeMap<String, Value>,
    pub(super) checkpoint: &'a CheckpointCoordinator,
    pub(super) budget_controller: &'a Option<RunBudgetController>,
    pub(super) persisted_denials: &'a [String],
}

pub(super) fn finalize_tool_cycle<C: LlmClient + Clone + 'static>(
    request: ToolCycleFinalization<'_, C>,
) -> Option<AgentResult> {
    let ToolCycleFinalization {
        runtime,
        controls,
        task,
        cycle_index,
        directive_result,
        completion_reason,
        completion_tool_name,
        messages,
        cycles,
        shared_state,
        checkpoint,
        budget_controller,
        persisted_denials,
    } = request;
    let native_outcome = if let Some(result) = directive_result {
        match result.directive {
            ToolDirective::WaitUser => NativeCycleOutcome {
                kind: NativeCycleOutcomeKind::WaitUser,
                completion_reason,
                completion_tool_name: completion_tool_name.map(str::to_string),
                steer_allowed: false,
            },
            ToolDirective::Finish => NativeCycleOutcome {
                kind: NativeCycleOutcomeKind::Completed,
                completion_reason,
                completion_tool_name: completion_tool_name.map(str::to_string),
                steer_allowed: cycle_index < task.max_cycles,
            },
            ToolDirective::Continue => unreachable!(),
        }
    } else if cycle_index >= task.max_cycles {
        NativeCycleOutcome {
            kind: NativeCycleOutcomeKind::MaxCycles,
            completion_reason: Some(CompletionReason::MaxCycles),
            completion_tool_name: None,
            steer_allowed: false,
        }
    } else {
        NativeCycleOutcome::continuing()
    };
    let completed_cycle = cycles
        .last()
        .expect("tool cycle was appended before after-cycle hooks")
        .clone();
    let decision = match runtime.apply_after_cycle_hooks(
        controls,
        task,
        &completed_cycle,
        messages,
        cycles,
        shared_state,
        native_outcome,
        persisted_denials,
    ) {
        Ok(decision) => decision,
        Err(result) => return Some(*result),
    };
    if decision
        .as_ref()
        .is_some_and(|decision| decision.action == AfterCycleAction::Steer)
    {
        return commit_nonterminal_cycle(
            checkpoint,
            task,
            cycle_index,
            messages,
            cycles,
            shared_state,
            budget_controller,
        );
    }
    if let Some(result) = directive_result {
        if let Some(result) = handle_directive_result(DirectiveResultRequest {
            runtime,
            controls,
            task,
            cycle_index,
            result,
            completion_reason: completion_reason
                .expect("terminal tool directive has a completion reason"),
            completion_tool_name: completion_tool_name
                .expect("terminal tool directive has a tool name"),
            messages,
            cycles,
            shared_state,
        }) {
            return Some(result);
        }
    }
    commit_nonterminal_cycle(
        checkpoint,
        task,
        cycle_index,
        messages,
        cycles,
        shared_state,
        budget_controller,
    )
}

#[allow(clippy::too_many_arguments)]
fn commit_nonterminal_cycle(
    checkpoint: &CheckpointCoordinator,
    task: &AgentTask,
    cycle_index: u32,
    messages: &[Message],
    cycles: &[CycleRecord],
    shared_state: &BTreeMap<String, Value>,
    budget_controller: &Option<RunBudgetController>,
) -> Option<AgentResult> {
    if cycle_index >= task.max_cycles {
        return None;
    }
    checkpoint.commit_cycle(cycle_index, messages, cycles, shared_state, || {
        budget_controller
            .as_ref()
            .map(RunBudgetController::snapshot)
    })
}

impl<C: LlmClient + Clone + 'static> AgentRuntime<C> {
    pub(super) fn after_cycle_hook_manager(&self) -> AfterCycleHookManager {
        AfterCycleHookManager::new(self.after_cycle_hooks.clone())
    }

    pub(super) fn read_after_cycle_denials(
        &self,
        controls: &RuntimeRunControls,
        cycle_index: u32,
        messages: &[Message],
        cycles: &[CycleRecord],
        shared_state: &BTreeMap<String, Value>,
    ) -> Result<Vec<String>, Box<AgentResult>> {
        read_after_cycle_disallowed_tools(shared_state).map_err(|error| {
            self.emit_log(
                controls,
                "after_cycle_failed",
                BTreeMap::from([
                    ("cycle".to_string(), Value::from(cycle_index)),
                    (
                        "error_code".to_string(),
                        Value::String(error.code.to_string()),
                    ),
                    ("error".to_string(), Value::String(error.to_string())),
                ]),
            );
            Box::new(failed_agent_result(
                messages.to_vec(),
                cycles.to_vec(),
                shared_state.clone(),
                format!("{}: {error}", error.code),
            ))
        })
    }

    pub(super) fn planned_tool_schemas_with_after_cycle_denials(
        &self,
        task: &AgentTask,
        persisted_denials: &[String],
    ) -> Vec<Value> {
        if persisted_denials.is_empty() {
            return self.planned_tool_schemas(task);
        }
        let mut projected = task.clone();
        let mut denials = metadata_disallowed_tools(task);
        denials.extend(persisted_denials.iter().cloned());
        denials.sort_by(|left, right| utf16_cmp(left, right));
        denials.dedup();
        projected.metadata.insert(
            "_vv_agent_disallowed_tools".to_string(),
            Value::Array(denials.into_iter().map(Value::String).collect()),
        );
        self.planned_tool_schemas(&projected)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn apply_after_cycle_hooks(
        &self,
        controls: &RuntimeRunControls,
        task: &AgentTask,
        cycle: &CycleRecord,
        messages: &mut Vec<Message>,
        cycles: &[CycleRecord],
        shared_state: &mut BTreeMap<String, Value>,
        native_outcome: NativeCycleOutcome,
        persisted_denials: &[String],
    ) -> Result<Option<AfterCycleDecision>, Box<AgentResult>> {
        let manager = self.after_cycle_hook_manager();
        if !manager.has_hooks() {
            return Ok(None);
        }

        let disallowed_tool_names = self.effective_after_cycle_denials(task, persisted_denials);
        let available_tool_names = self
            .planned_tool_schemas_with_after_cycle_denials(task, persisted_denials)
            .iter()
            .filter_map(|schema| {
                schema
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        let snapshot = AfterCycleSnapshot::capture(
            task.task_id.clone(),
            cycle.index,
            task.max_cycles,
            cycle,
            messages,
            shared_state,
            summarize_task_token_usage(cycles),
            available_tool_names,
            disallowed_tool_names,
            native_outcome.clone(),
        );
        let decision = manager.apply(&snapshot).map_err(|error| {
            self.emit_log(
                controls,
                "after_cycle_failed",
                BTreeMap::from([
                    ("cycle".to_string(), Value::from(cycle.index)),
                    (
                        "error_code".to_string(),
                        Value::String(error.code.to_string()),
                    ),
                    ("error".to_string(), Value::String(error.to_string())),
                ]),
            );
            Box::new(failed_agent_result(
                messages.clone(),
                cycles.to_vec(),
                shared_state.clone(),
                format!("{}: {error}", error.code),
            ))
        })?;

        if !decision.disallow_tools.is_empty() {
            persist_after_cycle_disallowed_tools(shared_state, &decision.disallow_tools).map_err(
                |error| {
                    self.emit_log(
                        controls,
                        "after_cycle_failed",
                        BTreeMap::from([
                            ("cycle".to_string(), Value::from(cycle.index)),
                            (
                                "error_code".to_string(),
                                Value::String(error.code.to_string()),
                            ),
                            ("error".to_string(), Value::String(error.to_string())),
                        ]),
                    );
                    Box::new(failed_agent_result(
                        messages.clone(),
                        cycles.to_vec(),
                        shared_state.clone(),
                        format!("{}: {error}", error.code),
                    ))
                },
            )?;
        }

        match decision.action {
            AfterCycleAction::StopNonSuccess => {
                let stop = decision.stop.as_ref().expect("validated stop payload");
                self.emit_log(
                    controls,
                    "after_cycle_stopped",
                    BTreeMap::from([
                        ("cycle".to_string(), Value::from(cycle.index)),
                        ("error_code".to_string(), Value::String(stop.code.clone())),
                        ("error".to_string(), Value::String(stop.message.clone())),
                    ]),
                );
                Err(Box::new(failed_agent_result(
                    messages.clone(),
                    cycles.to_vec(),
                    shared_state.clone(),
                    format!("{}: {}", stop.code, stop.message),
                )))
            }
            AfterCycleAction::Steer if !native_outcome.steer_allowed => {
                let code = "after_cycle_steer_unavailable";
                let message = "after-cycle steering is unavailable at this boundary";
                self.emit_log(
                    controls,
                    "after_cycle_failed",
                    BTreeMap::from([
                        ("cycle".to_string(), Value::from(cycle.index)),
                        ("error_code".to_string(), Value::String(code.to_string())),
                        ("error".to_string(), Value::String(message.to_string())),
                    ]),
                );
                Err(Box::new(failed_agent_result(
                    messages.clone(),
                    cycles.to_vec(),
                    shared_state.clone(),
                    format!("{code}: {message}"),
                )))
            }
            AfterCycleAction::Steer => {
                messages.extend(
                    decision
                        .steering_messages
                        .iter()
                        .cloned()
                        .map(Message::user),
                );
                self.emit_log(
                    controls,
                    "after_cycle_steered",
                    BTreeMap::from([
                        ("cycle".to_string(), Value::from(cycle.index)),
                        (
                            "steering_count".to_string(),
                            Value::from(decision.steering_messages.len() as u64),
                        ),
                        (
                            "disallowed_tools".to_string(),
                            Value::Array(
                                decision
                                    .disallow_tools
                                    .iter()
                                    .cloned()
                                    .map(Value::String)
                                    .collect(),
                            ),
                        ),
                    ]),
                );
                Ok(Some(decision))
            }
            AfterCycleAction::Continue => {
                self.emit_log(
                    controls,
                    "after_cycle_decision",
                    BTreeMap::from([
                        ("cycle".to_string(), Value::from(cycle.index)),
                        ("action".to_string(), Value::String("continue".to_string())),
                        (
                            "disallowed_tools".to_string(),
                            Value::Array(
                                decision
                                    .disallow_tools
                                    .iter()
                                    .cloned()
                                    .map(Value::String)
                                    .collect(),
                            ),
                        ),
                    ]),
                );
                Ok(Some(decision))
            }
        }
    }

    fn effective_after_cycle_denials(
        &self,
        task: &AgentTask,
        persisted_denials: &[String],
    ) -> Vec<String> {
        let mut denials = metadata_disallowed_tools(task);
        if let Some(policy) = &self.tool_policy {
            denials.extend(policy.disallowed_tools.iter().cloned());
        }
        denials.extend(persisted_denials.iter().cloned());
        denials.sort_by(|left, right| utf16_cmp(left, right));
        denials.dedup();
        denials
    }
}

fn metadata_disallowed_tools(task: &AgentTask) -> Vec<String> {
    task.metadata
        .get("_vv_agent_disallowed_tools")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        })
        .unwrap_or_default()
}
