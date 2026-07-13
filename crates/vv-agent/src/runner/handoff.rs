use std::sync::Arc;

use serde_json::Value;
use tokio::sync::broadcast;

use crate::agent::Agent;
use crate::events::{RunEvent, RunEventPayload};
use crate::result::RunResult;
use crate::run_config::RunConfig;

use super::support::{
    capture_event, effective_event_store, max_handoff_depth, HandoffRequest, SingleRunOutcome,
};
use super::{effective_session_id, NormalizedInput, Runner};

struct PendingHandoff {
    request: HandoffRequest,
    source_run_id: String,
    source_trace_id: String,
    session_id: Option<String>,
}

impl Runner {
    pub(super) fn run_agent_chain(
        &self,
        agent: &Agent,
        input: NormalizedInput,
        config: RunConfig,
        event_collector: Option<Arc<std::sync::Mutex<Vec<RunEvent>>>>,
        event_sender: Option<broadcast::Sender<RunEvent>>,
    ) -> Result<RunResult, String> {
        self.run_agent_chain_with_initial(agent, input, config, event_collector, event_sender, None)
    }

    pub(super) fn run_agent_chain_with_initial(
        &self,
        agent: &Agent,
        input: NormalizedInput,
        mut config: RunConfig,
        event_collector: Option<Arc<std::sync::Mutex<Vec<RunEvent>>>>,
        event_sender: Option<broadcast::Sender<RunEvent>>,
        initial_outcome: Option<SingleRunOutcome>,
    ) -> Result<RunResult, String> {
        let (event_store, event_store_fail_closed) =
            effective_event_store(&self.default_run_config, &config);
        let mut current_agent = agent.clone();
        let mut current_input = input;
        let max_handoffs = max_handoff_depth(&self.default_run_config, &config);
        let mut handoff_count = 0;
        let mut pending: Option<PendingHandoff> = None;
        let mut next_outcome = initial_outcome;
        loop {
            let outcome_result = if let Some(outcome) = next_outcome.take() {
                Ok(outcome)
            } else {
                self.run_single_agent(
                    &current_agent,
                    current_input.clone(),
                    config.clone(),
                    event_collector.clone(),
                    event_sender.clone(),
                )
            };
            let outcome = match outcome_result {
                Ok(outcome) => outcome,
                Err(error) => {
                    if let Some(pending) = pending.take() {
                        let mut event = RunEvent::new(
                            &pending.source_run_id,
                            &pending.source_trace_id,
                            &pending.request.from_agent,
                            Some(pending.request.cycle_index),
                            RunEventPayload::HandoffCompleted {
                                source_agent: pending.request.from_agent.clone(),
                                target_agent: pending.request.to_agent.clone(),
                                tool_call_id: pending.request.tool_call_id.clone(),
                            },
                        )
                        .with_handoff_lifecycle(
                            "failed",
                            pending.session_id.as_deref(),
                            None,
                        );
                        if let Some(session_id) = pending.session_id.as_deref() {
                            event = event.with_session_id(session_id);
                        }
                        for (key, value) in &pending.request.metadata {
                            event = event.with_metadata(key, value.clone());
                        }
                        event = event.with_metadata("chain_continues", Value::Bool(false));
                        event = event.with_metadata("error", Value::String(error.clone()));
                        capture_event(
                            event_collector.as_ref(),
                            event_sender.as_ref(),
                            event_store.as_ref(),
                            event_store_fail_closed,
                            event,
                        )?;
                    }
                    return Err(error);
                }
            };

            let next_handoff = outcome.handoff.clone();
            if let Some(pending) = pending.take() {
                let status = serde_json::to_value(outcome.result.status())
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_string))
                    .unwrap_or_else(|| "failed".to_string());
                let child_session_id = effective_session_id(&self.default_run_config, &config);
                let mut event = RunEvent::new(
                    &pending.source_run_id,
                    &pending.source_trace_id,
                    &pending.request.from_agent,
                    Some(pending.request.cycle_index),
                    RunEventPayload::HandoffCompleted {
                        source_agent: pending.request.from_agent.clone(),
                        target_agent: pending.request.to_agent.clone(),
                        tool_call_id: pending.request.tool_call_id.clone(),
                    },
                )
                .with_handoff_lifecycle(
                    status,
                    child_session_id.as_deref(),
                    Some(outcome.result.run_id()),
                );
                if let Some(session_id) = pending.session_id.as_deref() {
                    event = event.with_session_id(session_id);
                }
                for (key, value) in &pending.request.metadata {
                    event = event.with_metadata(key, value.clone());
                }
                event = event.with_metadata("chain_continues", Value::Bool(next_handoff.is_some()));
                capture_event(
                    event_collector.as_ref(),
                    event_sender.as_ref(),
                    event_store.as_ref(),
                    event_store_fail_closed,
                    event,
                )?;
            }

            let Some(handoff) = next_handoff else {
                let events = event_collector
                    .as_ref()
                    .and_then(|collector| collector.lock().ok().map(|events| events.clone()))
                    .unwrap_or_default();
                return Ok(outcome.result.with_events(events));
            };
            if handoff_count >= max_handoffs {
                return Err("maximum handoff depth exceeded".to_string());
            }
            let source_run_id = outcome.result.run_id().to_string();
            let source_trace_id = outcome.result.trace_id().to_string();
            let target = current_agent
                .handoffs()
                .iter()
                .find(|candidate| {
                    candidate.tool_name() == handoff.tool_name
                        && candidate.target().name() == handoff.to_agent
                })
                .map(|candidate| candidate.target().clone())
                .ok_or_else(|| {
                    format!(
                        "handoff target `{}` is not registered on agent `{}`",
                        handoff.to_agent,
                        current_agent.name()
                    )
                })?;
            let session_id = effective_session_id(&self.default_run_config, &config);
            let mut started = RunEvent::new(
                &source_run_id,
                &source_trace_id,
                handoff.from_agent.clone(),
                Some(handoff.cycle_index),
                RunEventPayload::HandoffStarted {
                    source_agent: handoff.from_agent.clone(),
                    target_agent: handoff.to_agent.clone(),
                    tool_call_id: handoff.tool_call_id.clone(),
                },
            )
            .with_handoff_lifecycle("started", session_id.as_deref(), None);
            if let Some(session_id) = session_id.as_deref() {
                started = started.with_session_id(session_id);
            }
            for (key, value) in &handoff.metadata {
                started = started.with_metadata(key, value.clone());
            }
            capture_event(
                event_collector.as_ref(),
                event_sender.as_ref(),
                event_store.as_ref(),
                event_store_fail_closed,
                started,
            )?;
            config.initial_shared_state = outcome.result.result().shared_state.clone();
            pending = Some(PendingHandoff {
                request: handoff,
                source_run_id,
                source_trace_id,
                session_id,
            });
            handoff_count += 1;
            current_input = NormalizedInput {
                text: pending
                    .as_ref()
                    .expect("pending handoff")
                    .request
                    .input
                    .clone(),
            };
            current_agent = target;
        }
    }
}
