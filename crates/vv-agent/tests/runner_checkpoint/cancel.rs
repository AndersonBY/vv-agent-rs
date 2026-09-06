use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::Value;
use vv_agent::{
    AmbiguousModelPolicy, AmbiguousToolPolicy, ControllerCommand, ControllerCommandVariant,
    ControllerHandle,
};

use super::*;

#[test]
fn checkpoint_defaults_match_contract10_ambiguity_policies() {
    let config = CheckpointConfig::default();
    assert_eq!(
        config.ambiguous_model_policy,
        AmbiguousModelPolicy::RetryWithDuplicateRisk
    );
    assert_eq!(
        config.ambiguous_tool_policy,
        AmbiguousToolPolicy::SurfaceToModel
    );
}

#[tokio::test]
async fn live_cancel_wins_checkpointed_terminal_finalize_and_retains_control_events() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint_key = "runner-live-cancel-finalize";
    let session = MemorySession::new("runner-live-cancel-session");
    let cancel_once = Arc::new(AtomicBool::new(false));
    let store_for_callback = store.clone();
    let cancel_for_callback = cancel_once.clone();
    let cancel_before_cycle =
        move |cycle: u32, _messages: &[vv_agent::Message], _state: &BTreeMap<String, Value>| {
            if cycle != 1 || cancel_for_callback.swap(true, Ordering::SeqCst) {
                return Vec::new();
            }
            let checkpoint = store_for_callback
                .load_checkpoint(checkpoint_key)
                .expect("load live checkpoint")
                .expect("live checkpoint");
            let handle = ControllerHandle::new(
                &checkpoint.checkpoint_key,
                &checkpoint.root_run_id,
                &checkpoint.trace_id,
            )
            .expect("controller handle");
            let command = ControllerCommand::new(
                "runner-live-cancel-finalize-command",
                handle,
                checkpoint.resume_attempt,
                checkpoint.revision,
                ControllerCommandVariant::Cancel,
            )
            .expect("cancel command");
            store_for_callback
                .resolve_controller_command(command)
                .expect("resolve live cancellation");
            vec![vv_agent::Message::user("cancelled by controller")]
        };
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "runner-live-cancel-model",
            vec![LLMResponse::new("normal terminal candidate")],
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("runner-live-cancel-agent")
        .instructions("Finish the run.")
        .model(ModelRef::named("runner-live-cancel-model"))
        .build()
        .expect("agent");

    let result = runner
        .run_with_config(
            &agent,
            "finish the run",
            RunConfig::builder()
                .session(session.clone())
                .before_cycle_messages(cancel_before_cycle)
                .checkpoint_config(checkpoint_config(store.clone(), checkpoint_key))
                .build(),
        )
        .await
        .expect("cancelled checkpointed run");
    assert!(cancel_once.load(Ordering::SeqCst));
    assert_eq!(result.status(), AgentStatus::Failed);
    assert_eq!(
        result.completion_reason(),
        Some(vv_agent::CompletionReason::Cancelled)
    );
    assert_eq!(result.error_code(), Some("cancelled_with_unknown_outcome"));

    let control_events = result
        .events()
        .iter()
        .filter_map(|event| match event.payload() {
            RunEventPayload::RunStateChanged { .. } => Some("run_state_changed"),
            RunEventPayload::RunCancelled { .. } => Some("run_cancelled"),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(control_events, ["run_state_changed", "run_cancelled"]);
    let terminal_sequence = result
        .events()
        .iter()
        .rev()
        .take(2)
        .map(|event| match event.payload() {
            RunEventPayload::SessionPersisted => "session_persisted",
            RunEventPayload::RunCancelled { .. } => "run_cancelled",
            _ => "other",
        })
        .collect::<Vec<_>>();
    assert_eq!(terminal_sequence, ["run_cancelled", "session_persisted"]);
    assert!(session
        .get_items(None)
        .await
        .expect("cancelled session items")
        .iter()
        .any(|item| item
            .to_message()
            .content
            .contains("cancelled by controller")));

    let terminal = store
        .load_checkpoint(checkpoint_key)
        .expect("load cancelled checkpoint")
        .expect("cancelled checkpoint");
    assert_eq!(terminal.status, CheckpointStatus::Failed);
    assert!(terminal.cancel_requested);
    let outbox_control_events = terminal
        .event_outbox
        .iter()
        .filter(|entry| {
            matches!(
                entry.event["type"].as_str(),
                Some("run_state_changed") | Some("run_cancelled")
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(outbox_control_events.len(), 2);
    assert!(outbox_control_events
        .iter()
        .all(|entry| entry.state == "delivered"));
}

async fn run_checkpointed_tool_cancellation(
    checkpoint_key: &str,
    definitive_receipt: bool,
) -> (vv_agent::RunResult, InMemoryCheckpointStore) {
    let store = InMemoryCheckpointStore::new();
    let token = vv_agent::CancellationToken::default();
    let token_for_tool = token.clone();
    let tool = StaticTool::new(
        "cancel_during_tool",
        "Cancel the run while the checkpointed tool is active.",
        json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false,
        }),
        Arc::new(move |context, _arguments| {
            token_for_tool.cancel_with_reason("cancelled in tool");
            if definitive_receipt {
                ToolExecutionResult::success(context.tool_call_id.clone(), "receipt won")
            } else {
                ToolExecutionResult::error(
                    context.tool_call_id.clone(),
                    "tool execution ended before a definitive receipt",
                )
                .with_error_code("tool_execution_failed")
            }
        }),
    );
    let provider = ScriptedModelProvider::new(
        "scripted",
        "checkpoint-cancel-model",
        vec![LLMResponse::with_tool_calls(
            "run the cancellable tool",
            vec![ToolCall::new(
                "call-cancel-during-tool",
                "cancel_during_tool",
                BTreeMap::new(),
            )],
        )],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("checkpoint-cancel-during-tool-agent")
        .instructions("Run the cancellable tool.")
        .model(ModelRef::named("checkpoint-cancel-model"))
        .tool(tool)
        .build()
        .expect("agent");
    let result = runner
        .run_with_config(
            &agent,
            "run the cancellable tool",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .cancellation_token(token)
                .checkpoint_config(checkpoint_config(store.clone(), checkpoint_key))
                .build(),
        )
        .await
        .expect("checkpointed cancellation run");
    (result, store)
}

#[tokio::test]
async fn direct_token_cancellation_closes_started_tool_durably() {
    let (result, store) =
        run_checkpointed_tool_cancellation("runner-token-cancel-mid-tool", false).await;

    assert_eq!(result.status(), AgentStatus::Failed);
    assert_eq!(
        result.completion_reason(),
        Some(vv_agent::CompletionReason::Cancelled)
    );
    assert_eq!(result.error_code(), Some("cancelled_with_unknown_outcome"));
    let checkpoint = store
        .load_checkpoint("runner-token-cancel-mid-tool")
        .expect("load cancelled checkpoint")
        .expect("cancelled checkpoint");
    assert_eq!(checkpoint.status, CheckpointStatus::Failed);
    assert!(checkpoint.terminal_result.is_some());
    assert!(checkpoint.cancel_requested);
    assert!(checkpoint.claim_token.is_none());
    assert_eq!(checkpoint.tool_journal.len(), 1);
    assert_eq!(checkpoint.tool_journal[0].state, OperationState::Failed);
    assert_eq!(
        checkpoint.tool_journal[0]
            .error
            .as_ref()
            .map(|error| error.code.as_str()),
        Some("tool_cancelled")
    );
    assert!(checkpoint.tool_journal[0].resume_observation.is_some());
    assert_eq!(
        checkpoint
            .terminal_result
            .as_ref()
            .and_then(|result| result["resume_observations"].as_array())
            .map(Vec::len),
        Some(1)
    );
    assert!(checkpoint
        .event_outbox
        .iter()
        .any(|entry| entry.event["type"] == "cycle_aborted"));
    assert!(result
        .events()
        .iter()
        .any(|event| matches!(event.payload(), RunEventPayload::RunCancelled { .. })));
}

#[tokio::test]
async fn direct_token_cancellation_keeps_a_previously_submitted_receipt() {
    let (result, store) =
        run_checkpointed_tool_cancellation("runner-token-cancel-receipt-wins", true).await;

    assert_eq!(result.status(), AgentStatus::Failed);
    assert_eq!(result.error_code(), Some("cancelled_with_unknown_outcome"));
    let checkpoint = store
        .load_checkpoint("runner-token-cancel-receipt-wins")
        .expect("load cancelled checkpoint")
        .expect("cancelled checkpoint");
    assert_eq!(checkpoint.tool_journal.len(), 1);
    assert_eq!(checkpoint.tool_journal[0].state, OperationState::Succeeded);
    assert!(checkpoint.tool_journal[0].result_digest.is_some());
    assert!(checkpoint.tool_journal[0]
        .error
        .as_ref()
        .is_none_or(|error| error.code != "tool_cancelled"));
    let completions = checkpoint
        .event_outbox
        .iter()
        .filter(|entry| {
            entry.event["type"] == "tool_call_completed"
                && entry.event["tool_call_id"] == "call-cancel-during-tool"
        })
        .collect::<Vec<_>>();
    assert_eq!(completions.len(), 1);
    assert_eq!(completions[0].state, "delivered");
    assert_eq!(
        result
            .events()
            .iter()
            .filter(|event| {
                matches!(
                    event.payload(),
                    RunEventPayload::ToolCallCompleted { tool_call_id, .. }
                        if tool_call_id == "call-cancel-during-tool"
                )
            })
            .count(),
        1
    );
}
