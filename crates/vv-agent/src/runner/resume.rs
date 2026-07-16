use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::budget::{BudgetEnforcementBoundary, BudgetEvaluator};
use crate::events::{RunEvent, RunEventPayload};
use crate::result::{PendingToolApproval, RunResult, RunResumeContext, RunState};
use crate::run_config::INITIAL_BUDGET_USAGE_METADATA_KEY;
use crate::types::{
    last_assistant_output, AgentResult, AgentStatus, CompletionReason, ToolDirective,
};

use super::helpers::terminal_event;
use super::session_blocking::block_on_session;
use super::support::{
    apply_cancellation_precedence, apply_output_guardrails, capture_event, effective_event_store,
    extract_handoff, SingleRunOutcome,
};
use super::{effective_session_id, NormalizedInput, Runner};

impl Runner {
    pub async fn resume(&self, state: RunState) -> Result<RunResult, String> {
        self.resume_with_optional_input(state, None).await
    }

    pub async fn resume_with_input(
        &self,
        state: RunState,
        input: impl Into<NormalizedInput>,
    ) -> Result<RunResult, String> {
        self.resume_with_optional_input(state, Some(input.into()))
            .await
    }

    async fn resume_with_optional_input(
        &self,
        state: RunState,
        input: Option<NormalizedInput>,
    ) -> Result<RunResult, String> {
        let (source, approved_ids, approval_consumption) = state.into_inner();
        let Some(resume_context) = source.resume_context().cloned() else {
            return Err("run state does not include resume context".to_string());
        };
        let origin_runner = resume_context.runner.clone();
        if let Some(result) = Box::pin(origin_runner.resume_approved_tool_call(
            &source,
            &resume_context,
            &approved_ids,
            &approval_consumption,
            input.as_ref(),
        ))
        .await
        {
            return result;
        }
        let mut config = resume_context.config;
        config.initial_messages = Some(source.result().messages.clone());
        config.initial_shared_state = source.result().shared_state.clone();
        set_initial_budget_usage(&mut config, source.budget_usage())?;
        let result = origin_runner
            .run_with_config(
                &resume_context.agent,
                input.unwrap_or(resume_context.input),
                config,
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(result)
    }

    async fn resume_approved_tool_call(
        &self,
        source: &RunResult,
        resume_context: &RunResumeContext,
        approved_ids: &[String],
        approval_consumption: &Arc<Mutex<std::collections::BTreeSet<String>>>,
        resume_input: Option<&NormalizedInput>,
    ) -> Option<Result<RunResult, String>> {
        let approval = match select_approved_tool_context(
            resume_context.pending_tool_approval.as_ref(),
            approved_ids,
        ) {
            Ok(Some(approval)) => approval.clone(),
            Ok(None) => return None,
            Err(error) => return Some(Err(error)),
        };
        if !approval_snapshot_matches_result(source.result(), &approval) {
            return Some(Err(
                "approved tool call does not match the captured interruption".to_string(),
            ));
        }
        if resume_input.is_some() {
            return Some(Err(
                "input cannot be provided when resuming an approved tool call".to_string(),
            ));
        }
        let cancellation_token = resume_context
            .config
            .cancellation_token
            .as_ref()
            .or(self.default_run_config.cancellation_token.as_ref());
        if cancellation_token.is_some_and(crate::runtime::CancellationToken::is_cancelled) {
            let mut cancelled = source.result().clone();
            cancelled.status = AgentStatus::Failed;
            cancelled.completion_reason = Some(CompletionReason::Cancelled);
            cancelled.completion_tool_name = None;
            cancelled.partial_output = cancelled
                .partial_output
                .or_else(|| last_assistant_output(&cancelled.cycles));
            cancelled.error = Some(
                cancellation_token
                    .and_then(crate::runtime::CancellationToken::reason)
                    .unwrap_or_else(|| "run cancelled".to_string()),
            );
            cancelled.budget_exhaustion = None;
            cancelled.final_answer = None;
            cancelled.wait_reason = None;
            return Some(self.finalize_approval_terminal(
                source,
                resume_context,
                &approval.interruption_id,
                cancelled,
                source.new_items().to_vec(),
                cancellation_token,
                None,
                Vec::new(),
            ));
        }
        {
            let mut consumed = approval_consumption
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !consumed.insert(approval.interruption_id.clone()) {
                return Some(Err("approval_already_consumed".to_string()));
            }
        }
        let resumed_run_id = format!("run_{}", uuid::Uuid::new_v4().simple());
        let budget_limits = resume_context
            .config
            .budget_limits
            .clone()
            .or_else(|| self.default_run_config.budget_limits.clone());
        let host_cost_meter = resume_context
            .config
            .host_cost_meter
            .clone()
            .or_else(|| self.default_run_config.host_cost_meter.clone());
        let mut budget_evaluator = match budget_limits.filter(|limits| limits.has_limits()) {
            Some(limits) => {
                match BudgetEvaluator::new(limits, host_cost_meter, source.budget_usage().cloned())
                {
                    Ok(evaluator) => Some(Box::new(evaluator)),
                    Err(error) => return Some(Err(error)),
                }
            }
            None => None,
        };
        let mut context = approval.context.clone();
        context.shared_state = source.result().shared_state.clone();
        let call = approval.call.clone();
        let tool_result = approval
            .orchestrator
            .run_one_with_approval(
                call.clone(),
                &mut context,
                approval.options.clone(),
                |_call, _requirement, _context| None,
            )
            .await
            .map_err(|error| error.to_string());
        let tool_result = match tool_result {
            Ok(result) => result,
            Err(error) => return Some(Err(error)),
        };
        let mut tool_result = approval.hook_manager.apply_after_tool_call(
            &approval.task,
            approval.cycle_index,
            &call,
            &context,
            tool_result,
        );
        let behavior_reason = crate::runtime::tool_call_runner::apply_tool_use_behavior(
            &approval.task,
            &call,
            &mut tool_result,
        );
        let mut agent_result = source.result().clone();
        agent_result.shared_state = context.shared_state.clone();
        if let Some(cycle) = agent_result
            .cycles
            .iter_mut()
            .find(|cycle| cycle.index == approval.cycle_index)
        {
            if let Some(existing) = cycle.tool_results.iter_mut().find(|existing| {
                existing.tool_call_id == call.id
                    && existing
                        .metadata
                        .get("approval_interruption_id")
                        .and_then(Value::as_str)
                        == Some(approval.interruption_id.as_str())
            }) {
                *existing = tool_result.clone();
            } else {
                cycle.tool_results.push(tool_result.clone());
            }
        }
        let tool_message = tool_result.to_message();
        agent_result.messages.retain(|message| {
            !(message.role == crate::types::MessageRole::Tool
                && message.tool_call_id.as_deref() == Some(call.id.as_str()))
        });
        agent_result.messages.push(tool_message.clone());
        if let Some(session) = resume_context.config.session.as_ref() {
            let session_items =
                crate::sessions::SessionItem::from_message(&tool_message).map(|item| vec![item]);
            let Some(session_items) = session_items else {
                return Some(Err(
                    "approved resume messages cannot be persisted to session".to_string(),
                ));
            };
            if let Err(error) = block_on_session(session.add_items(session_items)) {
                return Some(Err(error));
            }
        }
        let mut new_items = source
            .new_items()
            .iter()
            .filter(|message| {
                !(message.role == crate::types::MessageRole::Tool
                    && message.tool_call_id.as_deref() == Some(call.id.as_str()))
            })
            .cloned()
            .collect::<Vec<_>>();
        new_items.push(tool_message);

        let mut resume_budget_events = Vec::new();
        if let Some(evaluator) = &mut budget_evaluator {
            let observed_exhaustion = evaluator.tool_batch_complete(false);
            let snapshot = evaluator.snapshot();
            let cancelled =
                cancellation_token.is_some_and(crate::runtime::CancellationToken::is_cancelled);
            let exhaustion = (!cancelled).then_some(observed_exhaustion).flatten();
            agent_result.budget_usage = Some(snapshot.clone());
            agent_result.budget_exhaustion = exhaustion.clone();
            if exhaustion.is_some() {
                agent_result.status = AgentStatus::Failed;
                agent_result.completion_reason = Some(CompletionReason::BudgetExhausted);
                agent_result.completion_tool_name = None;
                agent_result.partial_output = last_assistant_output(&agent_result.cycles);
                agent_result.final_answer = None;
                agent_result.wait_reason = None;
                agent_result.error = Some("Run budget exhausted.".to_string());
            }
            let payload = match exhaustion.clone() {
                Some(budget_exhaustion) => RunEventPayload::BudgetExhausted {
                    enforcement_boundary: BudgetEnforcementBoundary::ToolBatchComplete,
                    budget_usage: snapshot,
                    budget_exhaustion,
                },
                None => RunEventPayload::BudgetSnapshot {
                    enforcement_boundary: BudgetEnforcementBoundary::ToolBatchComplete,
                    budget_usage: snapshot,
                },
            };
            let mut budget_event = RunEvent::new(
                &resumed_run_id,
                source.trace_id(),
                source.agent_name(),
                Some(approval.cycle_index),
                payload,
            );
            let session_id = effective_session_id(&self.default_run_config, &resume_context.config);
            if let Some(session_id) = session_id.as_deref() {
                budget_event = budget_event.with_session_id(session_id);
            }
            let (event_store, event_store_fail_closed) =
                effective_event_store(&self.default_run_config, &resume_context.config);
            if let Err(error) = capture_event(
                None,
                None,
                event_store.as_ref(),
                event_store_fail_closed,
                budget_event.clone(),
            ) {
                return Some(Err(error));
            }
            resume_budget_events.push(budget_event);
        }

        if agent_result.completion_reason != Some(CompletionReason::BudgetExhausted)
            && tool_result.directive == ToolDirective::Continue
        {
            let mut config = resume_context.config.clone();
            config.initial_messages = Some(agent_result.messages.clone());
            config.initial_shared_state = agent_result.shared_state.clone();
            config.trace_id = Some(source.trace_id().to_string());
            if let Err(error) =
                set_initial_budget_usage(&mut config, agent_result.budget_usage.as_ref())
            {
                return Some(Err(error));
            }
            let mut prior_events = Vec::new();
            if !resume_budget_events.is_empty() {
                prior_events.extend_from_slice(source.events());
                prior_events.extend(resume_budget_events);
            }
            let result = self
                .run_with_config(&resume_context.agent, source.input().to_string(), config)
                .await
                .map(move |result| {
                    let mut events = prior_events;
                    events.extend_from_slice(result.events());
                    let mut metadata = result.metadata().clone();
                    metadata.insert("resumed".to_string(), Value::Bool(true));
                    metadata.insert(
                        "approved_interruption_id".to_string(),
                        Value::String(approval.interruption_id.clone()),
                    );
                    result.with_events(events).with_metadata(metadata)
                });
            return Some(result);
        }

        if agent_result.completion_reason != Some(CompletionReason::BudgetExhausted) {
            let completion_reason = behavior_reason.unwrap_or(match tool_result.directive {
                ToolDirective::Finish => CompletionReason::ToolFinish,
                ToolDirective::WaitUser => CompletionReason::WaitUser,
                ToolDirective::Continue => unreachable!(),
            });
            agent_result.completion_reason = Some(completion_reason);
            agent_result.completion_tool_name = Some(call.name.clone());
            agent_result.error = None;
            match tool_result.directive {
                ToolDirective::Finish => {
                    agent_result.status = AgentStatus::Completed;
                    agent_result.partial_output = None;
                    agent_result.final_answer =
                        Some(crate::runtime::extract_final_message(&tool_result));
                    agent_result.wait_reason = None;
                }
                ToolDirective::WaitUser => {
                    agent_result.status = AgentStatus::WaitUser;
                    agent_result.partial_output = last_assistant_output(&agent_result.cycles);
                    agent_result.final_answer = None;
                    agent_result.wait_reason =
                        Some(crate::runtime::extract_wait_reason(&tool_result));
                }
                ToolDirective::Continue => unreachable!(),
            }
        }
        let guardrail_context = context
            .run_context
            .clone()
            .unwrap_or_else(|| crate::RunContext {
                run_id: source.run_id().to_string(),
                agent_name: resume_context.agent.name().to_string(),
                metadata: source.metadata().clone(),
                ..crate::RunContext::default()
            });
        agent_result =
            apply_output_guardrails(&resume_context.agent, &guardrail_context, agent_result);
        agent_result = apply_cancellation_precedence(agent_result, cancellation_token);
        let output_validation_error = agent_result
            .final_answer
            .as_deref()
            .filter(|_| agent_result.status == AgentStatus::Completed)
            .and_then(|output| {
                resume_context
                    .agent
                    .validate_output(output)
                    .err()
                    .map(|error| {
                        format!(
                            "failed to validate final output for agent `{}` as `{}`: {error}",
                            resume_context.agent.name(),
                            resume_context
                                .agent
                                .output_type_name()
                                .unwrap_or("configured output type")
                        )
                    })
            });
        let mut resumed = match self.finalize_approval_terminal(
            source,
            resume_context,
            &approval.interruption_id,
            agent_result,
            new_items,
            cancellation_token,
            Some(resumed_run_id),
            resume_budget_events,
        ) {
            Ok(resumed) => resumed,
            Err(error) => return Some(Err(error)),
        };
        if let Some(error) = output_validation_error {
            return Some(Err(error));
        }
        let session_id = effective_session_id(&self.default_run_config, &resume_context.config);
        let Some(handoff) = extract_handoff(resumed.result()) else {
            return Some(Ok(resumed));
        };

        let event_collector = Arc::new(Mutex::new(resumed.events().to_vec()));
        let mut legacy_event = RunEvent::new(
            resumed.run_id(),
            resumed.trace_id(),
            &handoff.from_agent,
            Some(handoff.cycle_index),
            RunEventPayload::Handoff {
                source_agent: handoff.from_agent.clone(),
                target_agent: handoff.to_agent.clone(),
                tool_call_id: handoff.tool_call_id.clone(),
            },
        );
        if let Some(session_id) = session_id.as_deref() {
            legacy_event = legacy_event.with_session_id(session_id);
        }
        for (key, value) in &handoff.metadata {
            legacy_event = legacy_event.with_metadata(key, value.clone());
        }
        let (event_store, event_store_fail_closed) =
            effective_event_store(&self.default_run_config, &resume_context.config);
        if let Err(error) = capture_event(
            Some(&event_collector),
            None,
            event_store.as_ref(),
            event_store_fail_closed,
            legacy_event,
        ) {
            return Some(Err(error));
        }
        let events = event_collector
            .lock()
            .map(|events| events.clone())
            .unwrap_or_default();
        resumed = resumed.with_events(events);
        let initial_outcome = SingleRunOutcome {
            result: resumed,
            handoff: Some(handoff),
        };
        let runner = self.clone();
        let agent = resume_context.agent.clone();
        let input = resume_context.input.clone();
        let config = resume_context.config.clone();
        Some(
            tokio::task::spawn_blocking(move || {
                runner.run_agent_chain_with_initial(
                    &agent,
                    input,
                    config,
                    Some(event_collector),
                    None,
                    Some(initial_outcome),
                )
            })
            .await
            .map_err(|error| format!("resume handoff task failed: {error}"))
            .and_then(|result| result),
        )
    }

    #[allow(clippy::too_many_arguments)] // Keep the terminal commit context explicit and atomic.
    fn finalize_approval_terminal(
        &self,
        source: &RunResult,
        resume_context: &RunResumeContext,
        interruption_id: &str,
        agent_result: AgentResult,
        new_items: Vec<crate::types::Message>,
        cancellation_token: Option<&crate::runtime::CancellationToken>,
        resumed_run_id: Option<String>,
        additional_events: Vec<RunEvent>,
    ) -> Result<RunResult, String> {
        let resumed_run_id =
            resumed_run_id.unwrap_or_else(|| format!("run_{}", uuid::Uuid::new_v4().simple()));
        let mut events = source.events().to_vec();
        events.extend(additional_events);
        let mut resumed = RunResult::new(
            resume_context.agent.name().to_string(),
            agent_result,
            source
                .resolved_model()
                .cloned()
                .expect("interrupted runs have a resolved model"),
        )
        .with_ids(&resumed_run_id, source.trace_id())
        .with_input(source.input())
        .with_new_items(new_items)
        .with_events(events)
        .with_metadata({
            let mut metadata = source.metadata().clone();
            metadata.insert("resumed".to_string(), Value::Bool(true));
            metadata.insert(
                "approved_interruption_id".to_string(),
                Value::String(interruption_id.to_string()),
            );
            metadata
        })
        .with_resume_context(resume_context.clone());
        let event_collector = Arc::new(Mutex::new(resumed.events().to_vec()));
        let session_id = effective_session_id(&self.default_run_config, &resume_context.config);
        let (event_store, event_store_fail_closed) =
            effective_event_store(&self.default_run_config, &resume_context.config);
        capture_event(
            Some(&event_collector),
            None,
            event_store.as_ref(),
            event_store_fail_closed,
            terminal_event(
                resumed.result(),
                resumed.run_id(),
                resumed.trace_id(),
                resume_context.agent.name(),
                session_id.as_deref(),
                cancellation_token,
            ),
        )?;
        let events = event_collector
            .lock()
            .map(|events| events.clone())
            .unwrap_or_default();
        resumed = resumed.with_events(events);
        Ok(resumed)
    }
}

fn set_initial_budget_usage(
    config: &mut crate::run_config::RunConfig,
    usage: Option<&crate::budget::BudgetUsageSnapshot>,
) -> Result<(), String> {
    match usage {
        Some(usage) => {
            let value = serde_json::to_value(usage)
                .map_err(|error| format!("failed to serialize resumed budget usage: {error}"))?;
            config
                .metadata
                .insert(INITIAL_BUDGET_USAGE_METADATA_KEY.to_string(), value);
        }
        None => {
            config.metadata.remove(INITIAL_BUDGET_USAGE_METADATA_KEY);
        }
    }
    Ok(())
}

fn select_approved_tool_context<'a>(
    pending: Option<&'a PendingToolApproval>,
    approved_ids: &[String],
) -> Result<Option<&'a PendingToolApproval>, String> {
    if approved_ids.is_empty() {
        return Ok(None);
    }
    let pending = pending.ok_or_else(|| {
        "approved tool call is missing its captured interruption context".to_string()
    })?;
    if !approved_ids.iter().any(|id| id == &pending.interruption_id) {
        return Err("approved tool call is missing its captured interruption context".to_string());
    }
    Ok(Some(pending))
}

fn approval_snapshot_matches_result(result: &AgentResult, approval: &PendingToolApproval) -> bool {
    result.cycles.iter().any(|cycle| {
        cycle.index == approval.cycle_index
            && cycle.tool_calls.iter().any(|call| call == &approval.call)
            && cycle.tool_results.iter().any(|tool_result| {
                tool_result.tool_call_id == approval.call.id
                    && tool_result
                        .metadata
                        .get("approval_interruption_id")
                        .and_then(Value::as_str)
                        == Some(approval.interruption_id.as_str())
                    && tool_result
                        .metadata
                        .get("tool_name")
                        .and_then(Value::as_str)
                        == Some(approval.call.name.as_str())
                    && tool_result.metadata.get("arguments")
                        == Some(&Value::Object(
                            approval.call.arguments.clone().into_iter().collect(),
                        ))
            })
    })
}

#[cfg(test)]
mod tests {
    use super::select_approved_tool_context;

    #[test]
    fn approved_id_without_captured_context_fails_closed() {
        let error = match select_approved_tool_context(None, &["approval_1".to_string()]) {
            Ok(_) => panic!("missing context must fail"),
            Err(error) => error,
        };
        assert_eq!(
            error,
            "approved tool call is missing its captured interruption context"
        );
    }

    #[test]
    fn conversational_resume_without_approved_id_needs_no_approval_context() {
        assert!(select_approved_tool_context(None, &[])
            .expect("conversational resume")
            .is_none());
    }
}
