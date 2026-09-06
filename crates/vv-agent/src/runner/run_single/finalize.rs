use super::*;
use crate::runner::run_single_entry::result_terminal_flags;
use crate::runtime::cancellation::CancellationToken;

pub(super) fn prepare_checkpoint_result(
    checkpoint_controller: Option<&CheckpointController>,
    terminal_replayed: bool,
    result: AgentResult,
    event_store_error: &Arc<Mutex<Option<String>>>,
    agent: &Agent,
    run_context: &RunContext,
    cancellation_token: Option<&CancellationToken>,
) -> Result<(AgentResult, bool, bool, bool, bool), String> {
    let (mut result, terminal_replayed) =
        replay_checkpoint_terminal(checkpoint_controller, terminal_replayed, result)?;
    if let Some(error) = event_store_error
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        return Err(error);
    }
    result = prepare_checkpoint_terminal(checkpoint_controller, terminal_replayed, result)?;
    let (reconciliation_required, operator_abort, deferred) = result_terminal_flags(&result);
    if !terminal_replayed && !reconciliation_required && !operator_abort {
        result = apply_output_guardrails(agent, run_context, result);
        result = apply_cancellation_precedence(result, cancellation_token);
    }
    Ok((
        result,
        terminal_replayed,
        reconciliation_required,
        operator_abort,
        deferred,
    ))
}

pub(super) fn validate_checkpoint_result(
    agent: &Agent,
    run_context: &RunContext,
    result: AgentResult,
    terminal_replayed: bool,
    reconciliation_required: bool,
    operator_abort: bool,
) -> (AgentResult, Option<String>) {
    let output_type_validation_error = if !reconciliation_required && !operator_abort {
        output_type_validation_error(agent, &result)
    } else {
        None
    };
    if terminal_replayed {
        (result, output_type_validation_error)
    } else {
        apply_optional_output_validation(agent, run_context, result, output_type_validation_error)
    }
}

pub(super) fn checkpoint_result_new_items(
    result: &AgentResult,
    session_result_prefix_len: usize,
    terminal_replayed: bool,
    reconciliation_required: bool,
    deferred: bool,
) -> Vec<crate::types::Message> {
    if terminal_replayed || reconciliation_required || deferred {
        Vec::new()
    } else {
        result
            .messages
            .get(session_result_prefix_len..)
            .unwrap_or_default()
            .to_vec()
    }
}

pub(super) fn close_checkpoint_controller(checkpoint_controller: Option<&CheckpointController>) {
    if let Some(controller) = checkpoint_controller {
        controller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .close();
    }
}
