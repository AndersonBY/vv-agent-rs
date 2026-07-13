use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::events::{RunEvent, RunEventPayload};
use crate::result::{PendingToolApproval, RunResult, RunResumeContext, RunState};
use crate::types::{AgentResult, ToolResultStatus};

use super::session_blocking::block_on_session;
use super::support::{capture_event, effective_event_store, extract_handoff, SingleRunOutcome};
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
        if let Some(result) = origin_runner
            .resume_approved_tool_call(
                &source,
                &resume_context,
                &approved_ids,
                &approval_consumption,
            )
            .await
        {
            return result;
        }
        let mut config = resume_context.config;
        config.initial_messages = Some(source.result().messages.clone());
        config.initial_shared_state = source.result().shared_state.clone();
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
        {
            let mut consumed = approval_consumption
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !consumed.insert(approval.interruption_id.clone()) {
                return Some(Err("approval_already_consumed".to_string()));
            }
        }
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
        let tool_result = approval.hook_manager.apply_after_tool_call(
            &approval.task,
            approval.cycle_index,
            &call,
            &context,
            tool_result,
        );
        if tool_result.status != ToolResultStatus::Success {
            return Some(Err(tool_result.content));
        }
        let mut agent_result = source.result().clone();
        agent_result.shared_state = context.shared_state.clone();
        agent_result.status = crate::types::AgentStatus::Completed;
        agent_result.final_answer = Some(tool_result.content.clone());
        agent_result.wait_reason = None;
        agent_result.error = None;
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
            let Some(session_item) = crate::sessions::SessionItem::from_message(&tool_message)
            else {
                return Some(Err(
                    "approved tool result cannot be persisted to session".to_string()
                ));
            };
            if let Err(error) = block_on_session(session.add_items(vec![session_item])) {
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
        let mut resumed = RunResult::new(
            resume_context.agent.name().to_string(),
            agent_result,
            source
                .resolved_model()
                .cloned()
                .expect("interrupted runs have a resolved model"),
        )
        .with_ids(source.run_id(), source.trace_id())
        .with_input(source.input())
        .with_new_items(new_items)
        .with_events(source.events().to_vec())
        .with_metadata({
            let mut metadata = source.metadata().clone();
            metadata.insert("resumed".to_string(), Value::Bool(true));
            metadata.insert(
                "approved_interruption_id".to_string(),
                Value::String(approval.interruption_id.clone()),
            );
            metadata
        })
        .with_resume_context(resume_context.clone());
        let Some(handoff) = extract_handoff(resumed.result()) else {
            return Some(Ok(resumed));
        };

        let event_collector = Arc::new(Mutex::new(resumed.events().to_vec()));
        let session_id = effective_session_id(&self.default_run_config, &resume_context.config);
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
