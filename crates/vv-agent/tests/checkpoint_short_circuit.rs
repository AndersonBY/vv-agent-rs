use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use vv_agent::{
    Agent, AgentStatus, BeforeToolCallEvent, BeforeToolCallPatch, CapabilityRef, CheckpointConfig,
    CheckpointStore, InMemoryCheckpointStore, LLMResponse, ModelRef, NoToolPolicy, ResumePolicy,
    RunConfig, RunEvent, RunEventPayload, Runner, RuntimeHook, ScriptedModelProvider, ToolCall,
    ToolExecutionResult,
};

fn checkpoint_config(store: InMemoryCheckpointStore, key: &str) -> CheckpointConfig {
    let mut config = CheckpointConfig::with_store(store);
    config.key = Some(key.to_string());
    config.resume_policy = ResumePolicy::ResumeIfPresent;
    config.capability_refs.insert(
        "before_cycle_messages".to_string(),
        CapabilityRef::new("runner.before-cycle", "1").expect("capability ref"),
    );
    config.capability_refs.insert(
        "session".to_string(),
        CapabilityRef::new("session.runner-checkpoint", "1").expect("capability ref"),
    );
    config
}

struct PlannedSuccessHook;

impl RuntimeHook for PlannedSuccessHook {
    fn before_tool_call(&self, event: BeforeToolCallEvent<'_>) -> Option<BeforeToolCallPatch> {
        Some(BeforeToolCallPatch {
            call: None,
            result: Some(ToolExecutionResult::success(
                event.call.id.clone(),
                "short-circuited success",
            )),
        })
    }
}

#[tokio::test]
async fn planned_unknown_tool_short_circuit_is_closed_before_cycle_commit() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint_key = "planned-unknown-tool-short-circuit";
    let observed_error_code = Arc::new(Mutex::new(None));
    let observer = observed_error_code.clone();
    let stream = Arc::new(move |event: &RunEvent| {
        if let RunEventPayload::ToolCallCompleted { error_code, .. } = event.payload() {
            *observer.lock().expect("tool result observer lock") = error_code.clone();
        }
    });
    let provider = ScriptedModelProvider::new(
        "scripted",
        "planned-short-circuit-model",
        vec![LLMResponse::with_tool_calls(
            "Call the unavailable tool.",
            vec![ToolCall::new(
                "call_missing",
                "missing_tool",
                BTreeMap::new(),
            )],
        )],
    );
    let workspace = tempfile::tempdir().expect("workspace");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(workspace.path())
        .build()
        .expect("runner");
    let agent = Agent::builder("planned-short-circuit-agent")
        .instructions("Call the unavailable tool.")
        .model(ModelRef::named("planned-short-circuit-model"))
        .build()
        .expect("agent");

    let result = runner
        .run_with_config(
            &agent,
            "exercise a planned tool failure",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .stream_arc(stream)
                .checkpoint_config(checkpoint_config(store.clone(), checkpoint_key))
                .build(),
        )
        .await
        .expect("planned short-circuit run must commit");
    assert_eq!(result.status(), AgentStatus::MaxCycles);

    assert_eq!(
        observed_error_code
            .lock()
            .expect("tool result observer lock")
            .as_deref(),
        Some("tool_not_allowed")
    );
    let committed = store
        .load_checkpoint(checkpoint_key)
        .expect("load committed checkpoint")
        .expect("committed checkpoint");
    assert!(committed.tool_journal.is_empty());
    assert!(committed.claim_token.is_none());
}

#[tokio::test]
async fn planned_success_short_circuit_commits_without_a_receipt() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint_key = "planned-success-short-circuit";
    let provider = ScriptedModelProvider::new(
        "scripted",
        "planned-success-model",
        vec![LLMResponse::with_tool_calls(
            "Call the short-circuited tool.",
            vec![ToolCall::new(
                "call_short_circuit_success",
                "missing_tool",
                BTreeMap::new(),
            )],
        )],
    );
    let workspace = tempfile::tempdir().expect("workspace");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(workspace.path())
        .build()
        .expect("runner");
    let agent = Agent::builder("planned-success-short-circuit-agent")
        .instructions("Call the short-circuited tool.")
        .model(ModelRef::named("planned-success-model"))
        .build()
        .expect("agent");
    let mut checkpoint = checkpoint_config(store.clone(), checkpoint_key);
    checkpoint.capability_refs.insert(
        "runtime_hook:0".to_string(),
        CapabilityRef::new("app-server.steering", "1").expect("capability ref"),
    );

    let result = runner
        .run_with_config(
            &agent,
            "exercise a planned tool success",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .hook(Arc::new(PlannedSuccessHook))
                .checkpoint_config(checkpoint)
                .build(),
        )
        .await
        .expect("planned short-circuit success must commit");
    assert_eq!(result.status(), AgentStatus::MaxCycles);

    let committed = store
        .load_checkpoint(checkpoint_key)
        .expect("load checkpoint")
        .expect("committed checkpoint");
    assert!(committed.tool_journal.is_empty());
    assert!(committed.claim_token.is_none());
    assert!(committed
        .event_outbox
        .iter()
        .all(|entry| entry.event["type"] != "tool_call_completed"));
}
