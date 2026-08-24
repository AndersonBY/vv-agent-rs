use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use vv_agent::{
    tool_request_digest, AfterCycleDecision, AfterCycleSnapshot, Agent, AgentStatus, CapabilityRef,
    Checkpoint, CheckpointConfig, CheckpointError, CheckpointStatus, CheckpointStore, ClaimMode,
    ContextError, ContextFragment, ContextProvider, ContextRequest, CycleDispatchResult,
    CycleDispatcher, DistributedBackend, DistributedCapabilities, DistributedCapabilityRegistry,
    DistributedCycleWorker, EventCursor, FunctionTool, InMemoryCheckpointStore, LLMResponse,
    MemorySession, MicrocompactionPolicy, ModelCallOperation, ModelRef, NoToolPolicy,
    OperationJournalEntry, OperationState, PromptBundle, PromptSection, ResumePolicy,
    RunBudgetLimits, RunConfig, RunEventPayload, Runner, RuntimeExecutionBackend, RuntimeRecipe,
    ScriptStep, ScriptedLlmClient, ScriptedModelProvider, Session, StaticTool, ThreadBackend,
    TokenUsage, ToolCall, ToolExecutionResult, ToolIdempotency, ToolMetadata, ToolOutput,
    ToolResultStatus, UsageSource,
};

#[derive(Clone)]
struct ClaimThenFailDispatcher {
    store: InMemoryCheckpointStore,
}

impl CycleDispatcher for ClaimThenFailDispatcher {
    fn dispatch_envelope(
        &self,
        envelope: &vv_agent::DistributedRunEnvelope,
    ) -> Result<CycleDispatchResult, String> {
        let key = &envelope.checkpoint_config.key;
        let now_ms = u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_millis(),
        )
        .expect("timestamp fits u64");
        self.store
            .claim_checkpoint(
                key,
                u64::from(envelope.cycle_index),
                "external-worker-claim",
                now_ms + 60_000,
                now_ms,
                ClaimMode::Continue,
            )
            .expect("claim checkpoint")
            .expect("external worker claim");
        Err("permanent transport failure after external claim".to_string())
    }
}

#[derive(Clone)]
struct DirectWorkerDispatcher {
    worker: Arc<DistributedCycleWorker>,
    fail_after_candidate_once: Option<Arc<AtomicBool>>,
    pending_after_candidate_loss_once: Option<Arc<AtomicBool>>,
}

impl CycleDispatcher for DirectWorkerDispatcher {
    fn dispatch_envelope(
        &self,
        envelope: &vv_agent::DistributedRunEnvelope,
    ) -> Result<CycleDispatchResult, String> {
        if self
            .pending_after_candidate_loss_once
            .as_ref()
            .is_some_and(|flag| flag.swap(false, Ordering::SeqCst))
        {
            return Ok(CycleDispatchResult::pending());
        }
        let result = self.worker.run_cycle(envelope.clone())?;
        if matches!(&result, CycleDispatchResult::TerminalCandidate { .. })
            && self
                .fail_after_candidate_once
                .as_ref()
                .is_some_and(|flag| flag.swap(false, Ordering::SeqCst))
        {
            if let Some(flag) = &self.pending_after_candidate_loss_once {
                flag.store(true, Ordering::SeqCst);
            }
            return Err(
                "retryable distributed delivery conflict: candidate acknowledgement lost"
                    .to_string(),
            );
        }
        Ok(result)
    }
}

fn checkpoint_config<S>(store: S, key: &str) -> CheckpointConfig
where
    S: CheckpointStore + 'static,
{
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

#[tokio::test]
async fn deferred_admission_failure_drops_staged_lifecycle_events() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint_key = "deferred-admission-failure";
    let deleted = Arc::new(AtomicBool::new(false));
    let effects = Arc::new(AtomicUsize::new(0));
    let lifecycle_events = Arc::new(Mutex::new(Vec::<String>::new()));
    let delete_store = store.clone();
    let delete_once = deleted.clone();
    let observed_lifecycle = lifecycle_events.clone();
    let stream = Arc::new(move |event: &vv_agent::RunEvent| {
        if matches!(event.payload(), RunEventPayload::ToolCallDeferred { .. }) {
            observed_lifecycle
                .lock()
                .expect("lifecycle events lock")
                .push("tool_call_deferred".to_string());
        }
        if matches!(event.payload(), RunEventPayload::ToolCallCompleted { .. }) {
            observed_lifecycle
                .lock()
                .expect("lifecycle events lock")
                .push("tool_call_completed".to_string());
        }
        if matches!(event.payload(), RunEventPayload::ToolCallStarted { .. })
            && !delete_once.swap(true, Ordering::SeqCst)
        {
            delete_store
                .delete_checkpoint(checkpoint_key)
                .expect("delete checkpoint before deferred admission");
        }
    });
    let effects_for_tool = effects.clone();
    let deferred_tool = StaticTool::new(
        "remote_write",
        "Record a durable external write.",
        json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false,
        }),
        Arc::new(move |context, _arguments| {
            effects_for_tool.fetch_add(1, Ordering::SeqCst);
            let _ = context.defer();
            ToolExecutionResult::success(context.tool_call_id.clone(), "not model-visible")
        }),
    );
    let provider = ScriptedModelProvider::new(
        "scripted",
        "deferred-admission-failure-model",
        vec![LLMResponse::with_tool_calls(
            "defer this write",
            vec![ToolCall::new(
                "call_deferred_failure",
                "remote_write",
                BTreeMap::new(),
            )],
        )],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("deferred-admission-failure-agent")
        .instructions("Defer the remote write.")
        .model(ModelRef::named("deferred-admission-failure-model"))
        .tool(deferred_tool)
        .build()
        .expect("agent");
    let error = match runner
        .run_with_config(
            &agent,
            "perform the write",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .stream_arc(stream)
                .checkpoint_config(checkpoint_config(store.clone(), checkpoint_key))
                .build(),
        )
        .await
    {
        Ok(_) => panic!("admission failure must stop the run before projecting lifecycle"),
        Err(error) => error,
    };
    assert!(deleted.load(Ordering::SeqCst));
    assert_eq!(
        effects.load(Ordering::SeqCst),
        1,
        "the provider effect occurred before the injected admission CAS failure"
    );
    assert!(
        error.contains("checkpoint_not_found"),
        "unexpected error: {error}"
    );
    assert!(
        lifecycle_events
            .lock()
            .expect("lifecycle events lock")
            .is_empty(),
        "failed admission must not project staged lifecycle"
    );
    assert!(
        store
            .load_checkpoint(checkpoint_key)
            .expect("load deleted checkpoint")
            .is_none(),
        "the injected CAS failure must happen before any durable lifecycle outbox write"
    );
}

#[tokio::test]
async fn resume_if_present_returns_deferred_pending_without_reclaiming_or_calling_model() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint_key = "deferred-resume-no-reclaim";
    let model_calls = Arc::new(AtomicUsize::new(0));
    let calls = model_calls.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "deferred-resume-model",
        vec![ScriptStep::callback(move |_request| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(LLMResponse::with_tool_calls(
                "defer this write",
                vec![ToolCall::new(
                    "call_deferred_resume",
                    "remote_write",
                    BTreeMap::new(),
                )],
            ))
        })],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .build()
        .expect("runner");
    let deferred_tool = StaticTool::new(
        "remote_write",
        "Record a durable external write.",
        json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false,
        }),
        Arc::new(|context, _arguments| {
            let _ = context.defer();
            ToolExecutionResult::success(context.tool_call_id.clone(), "not model-visible")
        }),
    );
    let agent = Agent::builder("deferred-resume-agent")
        .instructions("Defer the remote write.")
        .model(ModelRef::named("deferred-resume-model"))
        .tool(deferred_tool)
        .build()
        .expect("agent");
    let config = RunConfig::builder()
        .max_cycles(1)
        .no_tool_policy(NoToolPolicy::Finish)
        .checkpoint_config(checkpoint_config(store.clone(), checkpoint_key))
        .build();
    let first = runner
        .run_with_config(&agent, "perform the write", config.clone())
        .await
        .expect("first deferred run");
    assert_eq!(first.status(), AgentStatus::Deferred);
    assert_eq!(
        first.result().wait_reason.as_deref(),
        Some("deferred_pending")
    );
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);

    let second = runner
        .run_with_config(&agent, "perform the write", config)
        .await
        .expect("deferred resume");
    assert_eq!(second.status(), AgentStatus::Deferred);
    assert_eq!(
        second.result().wait_reason.as_deref(),
        Some("deferred_pending")
    );
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    let checkpoint = store
        .load_checkpoint(checkpoint_key)
        .expect("load checkpoint")
        .expect("checkpoint");
    assert_eq!(checkpoint.status, CheckpointStatus::Deferred);
    assert!(checkpoint.claim_token.is_none());
}

#[tokio::test]
async fn threaded_resume_if_present_returns_deferred_pending_without_reclaiming() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint_key = "threaded-deferred-resume-no-reclaim";
    let model_calls = Arc::new(AtomicUsize::new(0));
    let calls = model_calls.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "threaded-deferred-resume-model",
        vec![ScriptStep::callback(move |_request| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(LLMResponse::with_tool_calls(
                "defer this write",
                vec![ToolCall::new(
                    "call_threaded_deferred_resume",
                    "remote_write",
                    BTreeMap::new(),
                )],
            ))
        })],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .build()
        .expect("runner");
    let deferred_tool = StaticTool::new(
        "remote_write",
        "Record a durable external write.",
        json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false,
        }),
        Arc::new(|context, _arguments| {
            let _ = context.defer();
            ToolExecutionResult::success(context.tool_call_id.clone(), "not model-visible")
        }),
    );
    let agent = Agent::builder("threaded-deferred-resume-agent")
        .instructions("Defer the remote write.")
        .model(ModelRef::named("threaded-deferred-resume-model"))
        .tool(deferred_tool)
        .build()
        .expect("agent");
    let config = RunConfig::builder()
        .max_cycles(1)
        .no_tool_policy(NoToolPolicy::Finish)
        .execution_backend(RuntimeExecutionBackend::Thread(ThreadBackend::new(2)))
        .checkpoint_config(checkpoint_config(store.clone(), checkpoint_key))
        .build();

    let first = runner
        .run_with_config(&agent, "perform the write", config.clone())
        .await
        .expect("first deferred threaded run");
    assert_eq!(first.status(), AgentStatus::Deferred);
    assert_eq!(
        first.result().wait_reason.as_deref(),
        Some("deferred_pending")
    );
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);

    let second = runner
        .run_with_config(&agent, "perform the write", config)
        .await
        .expect("threaded deferred resume");
    assert_eq!(second.status(), AgentStatus::Deferred);
    assert_eq!(
        second.result().wait_reason.as_deref(),
        Some("deferred_pending")
    );
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    let checkpoint = store
        .load_checkpoint(checkpoint_key)
        .expect("load threaded checkpoint")
        .expect("threaded checkpoint");
    assert_eq!(checkpoint.status, CheckpointStatus::Deferred);
    assert!(checkpoint.claim_token.is_none());
}

#[tokio::test]
async fn run_definition_pins_after_cycle_hook_capability_slot() {
    let store = InMemoryCheckpointStore::new();
    let mut checkpoint = CheckpointConfig::with_store(store.clone());
    checkpoint.key = Some("after-cycle-definition".to_string());
    checkpoint.capability_refs.insert(
        "after_cycle_hook:0".to_string(),
        CapabilityRef::new("lifecycle.policy", "1").expect("capability ref"),
    );
    let hook =
        Arc::new(|_snapshot: &AfterCycleSnapshot| Ok(Some(AfterCycleDecision::continue_run())));
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "after-cycle-model",
            vec![LLMResponse::new("done")],
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("after-cycle-definition-agent")
        .instructions("Answer.")
        .model(ModelRef::named("after-cycle-model"))
        .build()
        .expect("agent");
    let config = RunConfig::builder()
        .max_cycles(1)
        .no_tool_policy(NoToolPolicy::Finish)
        .after_cycle_hook_arc(hook)
        .checkpoint_config(checkpoint)
        .build();

    let result = runner
        .run_with_config(&agent, "answer", config)
        .await
        .expect("run");

    assert_eq!(result.final_output(), Some("done"));
    let stored = store
        .load_checkpoint("after-cycle-definition")
        .expect("load")
        .expect("checkpoint");
    assert_eq!(
        stored.run_definition["capability_refs"]["after_cycle_hook:0"],
        json!({"id": "lifecycle.policy", "version": "1"})
    );
}

#[tokio::test]
async fn default_microcompaction_policy_freezes_without_capability_refs() {
    let store = InMemoryCheckpointStore::new();
    let mut checkpoint = CheckpointConfig::with_store(store.clone());
    checkpoint.key = Some("default-microcompaction-policy".to_string());
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "policy-model",
            vec![LLMResponse::new("done")],
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("policy-agent")
        .instructions("Answer.")
        .model(ModelRef::named("policy-model"))
        .build()
        .expect("agent");

    runner
        .run_with_config(
            &agent,
            "answer",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .checkpoint_config(checkpoint)
                .build(),
        )
        .await
        .expect("default checkpoint run");

    let stored = store
        .load_checkpoint("default-microcompaction-policy")
        .expect("load")
        .expect("checkpoint");
    assert_eq!(
        stored.run_definition["runtime_controls"]["microcompaction_policy"],
        serde_json::to_value(MicrocompactionPolicy::default()).expect("policy")
    );
    assert_eq!(stored.run_definition["capability_refs"], json!({}));
    assert!(stored.run_definition["run_metadata"]
        .get("microcompaction_policy")
        .is_none());
}

#[tokio::test]
async fn checkpoint_resume_keeps_frozen_microcompaction_policy() {
    let store = InMemoryCheckpointStore::new();
    let mut checkpoint = CheckpointConfig::with_store(store.clone());
    checkpoint.key = Some("frozen-microcompaction-policy".to_string());
    checkpoint.resume_policy = ResumePolicy::ResumeIfPresent;
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "policy-model",
            vec![LLMResponse::new("done")],
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("policy-agent")
        .instructions("Answer.")
        .model(ModelRef::named("policy-model"))
        .build()
        .expect("agent");
    let frozen = MicrocompactionPolicy::new(0.80, 0.55, 2, 700).expect("frozen policy");

    runner
        .run_with_config(
            &agent,
            "answer",
            RunConfig::builder()
                .max_cycles(1)
                .no_tool_policy(NoToolPolicy::Finish)
                .microcompaction_policy(frozen)
                .checkpoint_config(checkpoint.clone())
                .build(),
        )
        .await
        .expect("initial run");
    runner
        .run_with_config(
            &agent,
            "answer",
            RunConfig::builder()
                .microcompaction_policy(
                    MicrocompactionPolicy::new(0.90, 0.50, 0, 1).expect("caller policy"),
                )
                .checkpoint_config(checkpoint)
                .build(),
        )
        .await
        .expect("terminal replay");

    let stored = store
        .load_checkpoint("frozen-microcompaction-policy")
        .expect("load")
        .expect("checkpoint");
    assert_eq!(
        stored.run_definition["runtime_controls"]["microcompaction_policy"],
        serde_json::to_value(frozen).expect("policy")
    );
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TypedCheckpointOutput {
    answer: String,
}

#[tokio::test]
async fn terminal_replay_repeats_typed_output_validation_without_model_call() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let calls_for_model = model_calls.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "typed-checkpoint-model",
        vec![ScriptStep::callback(move |_request| {
            calls_for_model.fetch_add(1, Ordering::SeqCst);
            Ok(LLMResponse::new(r#"{"answer":42}"#))
        })],
    );
    let workspace = tempfile::tempdir().expect("workspace");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(workspace.path())
        .build()
        .expect("runner");
    let agent = Agent::builder("typed-checkpoint-agent")
        .instructions("Return typed JSON.")
        .model(ModelRef::named("typed-checkpoint-model"))
        .output_type::<TypedCheckpointOutput>()
        .build()
        .expect("agent");
    let store = InMemoryCheckpointStore::new();
    let mut checkpoint = checkpoint_config(store.clone(), "typed-checkpoint");
    checkpoint.capability_refs.insert(
        "output_validator".to_string(),
        CapabilityRef::new("typed-checkpoint-output", "1").expect("capability ref"),
    );
    let config = RunConfig::builder()
        .max_cycles(1)
        .no_tool_policy(NoToolPolicy::Finish)
        .checkpoint_config(checkpoint)
        .build();

    let first_error = match runner
        .run_with_config(&agent, "return invalid typed output", config.clone())
        .await
    {
        Ok(_) => panic!("initial typed output validation must fail"),
        Err(error) => error,
    };
    assert!(first_error.contains("failed to validate final output"));
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    let terminal = store
        .load_checkpoint("typed-checkpoint")
        .expect("load checkpoint")
        .expect("terminal checkpoint");
    assert_eq!(terminal.status, CheckpointStatus::Completed);
    assert!(terminal.terminal_result.is_some());

    let replay_error = match runner
        .run_with_config(&agent, "return invalid typed output", config)
        .await
    {
        Ok(_) => panic!("terminal replay must repeat typed output validation"),
        Err(error) => error,
    };
    assert!(replay_error.contains("failed to validate final output"));
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn distributed_worker_returns_candidate_and_runner_finalizes_once() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let model_calls_for_worker = model_calls.clone();
    let worker_llm = ScriptedLlmClient::from_steps(vec![ScriptStep::callback(move |_request| {
        model_calls_for_worker.fetch_add(1, Ordering::SeqCst);
        Ok(LLMResponse::new("done"))
    })]);
    let outer_provider = ScriptedModelProvider::new(
        "scripted",
        "distributed-checkpoint-model",
        vec![LLMResponse::new("outer provider must not execute")],
    );
    let store = InMemoryCheckpointStore::new();
    let checkpoint_ref =
        CapabilityRef::new("checkpoint.runner-distributed", "2").expect("checkpoint ref");
    let llm_ref = CapabilityRef::new("llm.runner-distributed", "1").expect("llm ref");
    let registry = DistributedCapabilityRegistry::new();
    registry.register_checkpoint_store(checkpoint_ref.clone(), Arc::new(store.clone()));
    registry.register_llm_client(llm_ref.clone(), Arc::new(worker_llm));
    let worker = Arc::new(DistributedCycleWorker::new(registry));
    let dispatcher = Arc::new(DirectWorkerDispatcher {
        worker,
        fail_after_candidate_once: None,
        pending_after_candidate_loss_once: None,
    });
    let mut recipe = RuntimeRecipe::new(
        "unused-settings.json",
        "scripted",
        "distributed-checkpoint-model",
        ".",
    );
    recipe.capabilities = DistributedCapabilities {
        llm_client_ref: Some(llm_ref),
        checkpoint_store_ref: Some(checkpoint_ref.clone()),
        ..DistributedCapabilities::default()
    };
    let backend = DistributedBackend::new(recipe, dispatcher);
    let validator_calls = Arc::new(AtomicUsize::new(0));
    let validator_calls_for_agent = validator_calls.clone();
    let agent = Agent::builder("distributed-checkpoint-agent")
        .instructions("Return done.")
        .model(ModelRef::named("distributed-checkpoint-model"))
        .output_validator("distributed-output", move |output| {
            validator_calls_for_agent.fetch_add(1, Ordering::SeqCst);
            (output == "done")
                .then_some(())
                .ok_or_else(|| "unexpected output".to_string())
        })
        .build()
        .expect("agent");
    let runner = Runner::builder()
        .model_provider(outer_provider)
        .workspace(tempfile::tempdir().expect("workspace").path())
        .build()
        .expect("runner");
    let session = MemorySession::new("distributed-checkpoint-session");
    let mut checkpoint = CheckpointConfig::with_store(store.clone());
    checkpoint.key = Some("runner-distributed-checkpoint".to_string());
    checkpoint.resume_policy = ResumePolicy::ResumeIfPresent;
    checkpoint
        .capability_refs
        .insert("checkpoint_store".to_string(), checkpoint_ref);
    checkpoint.capability_refs.insert(
        "session".to_string(),
        CapabilityRef::new("session.runner-distributed", "1").expect("session ref"),
    );
    checkpoint.capability_refs.insert(
        "output_validator".to_string(),
        CapabilityRef::new("output.runner-distributed", "1").expect("output ref"),
    );
    let config = RunConfig::builder()
        .max_cycles(1)
        .no_tool_policy(NoToolPolicy::Finish)
        .execution_backend(backend.into())
        .session(session.clone())
        .checkpoint_config(checkpoint)
        .build();

    let result = runner
        .run_with_config(&agent, "finish in the worker", config.clone())
        .await
        .expect("distributed run");

    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(result.final_output(), Some("done"));
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    assert_eq!(validator_calls.load(Ordering::SeqCst), 1);
    let terminal = store
        .load_checkpoint("runner-distributed-checkpoint")
        .expect("load terminal")
        .expect("terminal checkpoint");
    assert_eq!(terminal.status, CheckpointStatus::Completed);
    assert!(terminal.claim_token.is_none());
    assert!(terminal.terminal_result.is_some());
    assert!(terminal.terminal_acknowledged);
    let session_items = session.get_items(None).await.expect("session items");
    assert!(!session_items.is_empty());

    let replay = runner
        .run_with_config(&agent, "finish in the worker", config)
        .await
        .expect("terminal replay");
    assert_eq!(replay.status(), AgentStatus::Completed);
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    assert_eq!(validator_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        session
            .get_items(None)
            .await
            .expect("replayed session items"),
        session_items
    );
}

#[tokio::test]
async fn distributed_candidate_ack_loss_recovers_from_receipt_without_second_model_call() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let model_calls_for_worker = model_calls.clone();
    let worker_llm = ScriptedLlmClient::from_steps(vec![ScriptStep::callback(move |_request| {
        model_calls_for_worker.fetch_add(1, Ordering::SeqCst);
        Ok(LLMResponse::new("recovered"))
    })]);
    let outer_provider = ScriptedModelProvider::new(
        "scripted",
        "candidate-recovery-model",
        vec![LLMResponse::new("outer provider must not execute")],
    );
    let store = InMemoryCheckpointStore::new();
    let checkpoint_ref =
        CapabilityRef::new("checkpoint.candidate-recovery", "2").expect("checkpoint ref");
    let llm_ref = CapabilityRef::new("llm.candidate-recovery", "1").expect("llm ref");
    let registry = DistributedCapabilityRegistry::new();
    registry.register_checkpoint_store(checkpoint_ref.clone(), Arc::new(store.clone()));
    registry.register_llm_client(llm_ref.clone(), Arc::new(worker_llm));
    let lost_ack = Arc::new(AtomicBool::new(true));
    let pending_after_loss = Arc::new(AtomicBool::new(false));
    let dispatcher = Arc::new(DirectWorkerDispatcher {
        worker: Arc::new(DistributedCycleWorker::new(registry)),
        fail_after_candidate_once: Some(lost_ack.clone()),
        pending_after_candidate_loss_once: Some(pending_after_loss.clone()),
    });
    let mut recipe = RuntimeRecipe::new(
        "unused-settings.json",
        "scripted",
        "candidate-recovery-model",
        ".",
    );
    recipe.capabilities = DistributedCapabilities {
        llm_client_ref: Some(llm_ref),
        checkpoint_store_ref: Some(checkpoint_ref.clone()),
        ..DistributedCapabilities::default()
    };
    let backend = DistributedBackend::new(recipe, dispatcher)
        .with_lease_duration(Duration::from_secs(2))
        .with_dispatch_timeout(Duration::from_secs(10));
    let agent = Agent::builder("candidate-recovery-agent")
        .instructions("Return recovered.")
        .model(ModelRef::named("candidate-recovery-model"))
        .build()
        .expect("agent");
    let runner = Runner::builder()
        .model_provider(outer_provider)
        .workspace(tempfile::tempdir().expect("workspace").path())
        .build()
        .expect("runner");
    let mut checkpoint = CheckpointConfig::with_store(store.clone());
    checkpoint.key = Some("candidate-ack-loss".to_string());
    checkpoint.resume_policy = ResumePolicy::ResumeIfPresent;
    checkpoint
        .capability_refs
        .insert("checkpoint_store".to_string(), checkpoint_ref);
    let config = RunConfig::builder()
        .max_cycles(1)
        .no_tool_policy(NoToolPolicy::Finish)
        .execution_backend(backend.into())
        .checkpoint_config(checkpoint)
        .build();

    let result = runner
        .run_with_config(&agent, "recover candidate", config)
        .await
        .expect("recovered distributed run");

    assert_eq!(
        result.status(),
        AgentStatus::Completed,
        "distributed candidate recovery failed: {:?}",
        result.result().error
    );
    assert_eq!(result.final_output(), Some("recovered"));
    assert_eq!(result.result().cycles.len(), 1);
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    assert!(!lost_ack.load(Ordering::SeqCst));
    assert!(!pending_after_loss.load(Ordering::SeqCst));
    let terminal = store
        .load_checkpoint("candidate-ack-loss")
        .expect("load checkpoint")
        .expect("terminal checkpoint");
    assert_eq!(terminal.resume_attempt, 2);
    assert_eq!(terminal.status, CheckpointStatus::Completed);
    assert!(terminal.terminal_acknowledged);
}

#[tokio::test]
async fn distributed_dispatch_failure_preserves_root_error_and_external_claim() {
    let store = InMemoryCheckpointStore::new();
    let checkpoint_ref =
        CapabilityRef::new("checkpoint.dispatch-failure", "2").expect("checkpoint ref");
    let mut recipe = RuntimeRecipe::new(
        "unused-settings.json",
        "scripted",
        "dispatch-failure-model",
        ".",
    );
    recipe.capabilities = DistributedCapabilities {
        checkpoint_store_ref: Some(checkpoint_ref.clone()),
        ..DistributedCapabilities::default()
    };
    let backend = DistributedBackend::new(
        recipe,
        Arc::new(ClaimThenFailDispatcher {
            store: store.clone(),
        }),
    );
    let provider = ScriptedModelProvider::new(
        "scripted",
        "dispatch-failure-model",
        vec![LLMResponse::new("outer provider must not execute")],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(tempfile::tempdir().expect("workspace").path())
        .build()
        .expect("runner");
    let agent = Agent::builder("dispatch-failure-agent")
        .instructions("Return a result.")
        .model(ModelRef::named("dispatch-failure-model"))
        .build()
        .expect("agent");
    let mut checkpoint = CheckpointConfig::with_store(store.clone());
    checkpoint.key = Some("dispatch-failure".to_string());
    checkpoint.resume_policy = ResumePolicy::ResumeIfPresent;
    checkpoint
        .capability_refs
        .insert("checkpoint_store".to_string(), checkpoint_ref);
    let config = RunConfig::builder()
        .execution_backend(backend.into())
        .checkpoint_config(checkpoint)
        .build();

    let result = runner
        .run_with_config(&agent, "exercise dispatch failure", config)
        .await
        .expect("dispatch failure remains an observable run result");

    assert_eq!(result.status(), AgentStatus::Failed);
    assert_eq!(
        result.result().error.as_deref(),
        Some("checkpoint_dispatch_failed: permanent transport failure after external claim")
    );
    let persisted = store
        .load_checkpoint("dispatch-failure")
        .expect("load checkpoint")
        .expect("checkpoint remains durable");
    assert_eq!(persisted.status, CheckpointStatus::Running);
    assert_eq!(
        persisted.claim_token.as_deref(),
        Some("external-worker-claim")
    );
    assert!(persisted.terminal_result.is_none());
    assert!(!persisted.terminal_acknowledged);
}

#[tokio::test]
async fn distributed_execution_commits_nonterminal_cycle_before_max_cycles_candidate() {
    let model_calls = Arc::new(AtomicUsize::new(0));
    let first_calls = model_calls.clone();
    let second_calls = model_calls.clone();
    let worker_llm = ScriptedLlmClient::from_steps(vec![
        ScriptStep::callback(move |_request| {
            first_calls.fetch_add(1, Ordering::SeqCst);
            Ok(LLMResponse::new("cycle one"))
        }),
        ScriptStep::callback(move |_request| {
            second_calls.fetch_add(1, Ordering::SeqCst);
            Ok(LLMResponse::new("cycle two"))
        }),
    ]);
    let outer_provider = ScriptedModelProvider::new(
        "scripted",
        "distributed-multicycle-model",
        vec![LLMResponse::new("outer provider must not execute")],
    );
    let store = InMemoryCheckpointStore::new();
    let checkpoint_ref = CapabilityRef::new("checkpoint.distributed-multicycle", "2").unwrap();
    let llm_ref = CapabilityRef::new("llm.distributed-multicycle", "1").unwrap();
    let registry = DistributedCapabilityRegistry::new();
    registry.register_checkpoint_store(checkpoint_ref.clone(), Arc::new(store.clone()));
    registry.register_llm_client(llm_ref.clone(), Arc::new(worker_llm));
    let dispatcher = Arc::new(DirectWorkerDispatcher {
        worker: Arc::new(DistributedCycleWorker::new(registry)),
        fail_after_candidate_once: None,
        pending_after_candidate_loss_once: None,
    });
    let mut recipe = RuntimeRecipe::new(
        "unused-settings.json",
        "scripted",
        "distributed-multicycle-model",
        ".",
    );
    recipe.capabilities = DistributedCapabilities {
        llm_client_ref: Some(llm_ref),
        checkpoint_store_ref: Some(checkpoint_ref.clone()),
        ..DistributedCapabilities::default()
    };
    let backend = DistributedBackend::new(recipe, dispatcher);
    let runner = Runner::builder()
        .model_provider(outer_provider)
        .workspace(tempfile::tempdir().expect("workspace").path())
        .build()
        .expect("runner");
    let agent = Agent::builder("distributed-multicycle-agent")
        .instructions("Continue until the configured cycle budget ends.")
        .model(ModelRef::named("distributed-multicycle-model"))
        .build()
        .expect("agent");
    let mut checkpoint = CheckpointConfig::with_store(store.clone());
    checkpoint.key = Some("distributed-multicycle".to_string());
    checkpoint.resume_policy = ResumePolicy::ResumeIfPresent;
    checkpoint
        .capability_refs
        .insert("checkpoint_store".to_string(), checkpoint_ref);

    let result = runner
        .run_with_config(
            &agent,
            "run two cycles",
            RunConfig::builder()
                .max_cycles(2)
                .no_tool_policy(NoToolPolicy::Continue)
                .execution_backend(backend.into())
                .checkpoint_config(checkpoint)
                .build(),
        )
        .await
        .expect("distributed multicycle run");

    assert_eq!(result.status(), AgentStatus::MaxCycles);
    assert_eq!(result.result().cycles.len(), 2);
    assert_eq!(model_calls.load(Ordering::SeqCst), 2);
    let terminal = store
        .load_checkpoint("distributed-multicycle")
        .unwrap()
        .unwrap();
    assert_eq!(terminal.status, CheckpointStatus::MaxCycles);
    assert_eq!(terminal.cycle_index, 2);
    assert_eq!(terminal.cycles.len(), 2);
    assert!(terminal.terminal_acknowledged);
}

#[path = "runner_checkpoint/deferred.rs"]
mod deferred;
#[path = "runner_checkpoint/resume.rs"]
mod resume;
#[path = "runner_checkpoint/session_memory.rs"]
mod session_memory;
