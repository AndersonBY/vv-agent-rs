use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::checkpoint::{CheckpointConfig, CheckpointResult, ClaimMode, EventCursor};
use crate::runtime::checkpoint_resume::{CheckpointControllerRequest, CheckpointResumeController};
use crate::runtime::engine::controls::CheckpointRuntimeControl;
use crate::runtime::run_definition::{build_run_definition, RunDefinitionRequest};
use crate::runtime::state::{Checkpoint, CheckpointHistoryRecords, CheckpointStore};
use crate::{
    Agent, AgentStatus, CancellationToken, InMemoryCheckpointStore, LLMResponse, ModelSettings,
    NoToolPolicy, PromptBundle, RunConfig, RunEvent, RunEventPayload, ScriptedLlmClient,
    TokenUsage, UsageSource,
};

#[derive(Clone)]
struct ObservedStore {
    inner: Arc<dyn CheckpointStore>,
    history_reads: Arc<AtomicUsize>,
    fail_after_fourth_commit: Arc<AtomicBool>,
}

impl Default for ObservedStore {
    fn default() -> Self {
        Self {
            inner: Arc::new(InMemoryCheckpointStore::new()),
            history_reads: Arc::new(AtomicUsize::new(0)),
            fail_after_fourth_commit: Arc::new(AtomicBool::new(false)),
        }
    }
}

macro_rules! delegate {
    ($($method:ident($($arg:ident: $type:ty),*) -> $result:ty;)+) => {
        $(fn $method(&self, $($arg: $type),*) -> $result {
            self.inner.$method($($arg),*)
        })+
    };
}

impl CheckpointStore for ObservedStore {
    fn load_checkpoint_history(&self, key: &str) -> CheckpointResult<CheckpointHistoryRecords> {
        self.history_reads.fetch_add(1, Ordering::SeqCst);
        self.inner.load_checkpoint_history(key)
    }

    fn commit_checkpoint(
        &self,
        checkpoint: Checkpoint,
        token: &str,
        revision: u64,
    ) -> CheckpointResult<bool> {
        let cycle_index = checkpoint.cycle_index;
        let committed = self.inner.commit_checkpoint(checkpoint, token, revision)?;
        if committed
            && cycle_index == 4
            && self.fail_after_fourth_commit.swap(false, Ordering::SeqCst)
        {
            return Err(crate::CheckpointError::new(
                "test_crash_after_commit",
                "injected failure after committed cycle four",
            ));
        }
        Ok(committed)
    }

    delegate! {
        create_checkpoint(checkpoint: Checkpoint) -> CheckpointResult<bool>;
        load_checkpoint(key: &str) -> CheckpointResult<Option<Checkpoint>>;
        claim_checkpoint(key: &str, cycle: u64, token: &str, expiry: u64, now: u64, mode: ClaimMode) -> CheckpointResult<Option<Checkpoint>>;
        progress_checkpoint(checkpoint: Checkpoint, token: &str, revision: u64) -> CheckpointResult<bool>;
        suspend_checkpoint(checkpoint: Checkpoint, token: &str, revision: u64) -> CheckpointResult<bool>;
        finalize_claimed_checkpoint(checkpoint: Checkpoint, token: &str, revision: u64) -> CheckpointResult<bool>;
        finalize_checkpoint(checkpoint: Checkpoint, revision: u64) -> CheckpointResult<bool>;
        renew_checkpoint_claim(key: &str, token: &str, expiry: u64, now: u64) -> CheckpointResult<crate::checkpoint::CheckpointRenewalOutcome>;
        record_event_delivery(key: &str, token: Option<&str>, revision: u64, event_id: &str, digest: &str, cursor: EventCursor) -> CheckpointResult<bool>;
        acknowledge_terminal(key: &str, revision: u64) -> CheckpointResult<bool>;
        delete_checkpoint(key: &str) -> CheckpointResult<()>;
        list_checkpoints() -> CheckpointResult<Vec<String>>;
    }
}

fn run_archived_history(cancel_before_fifth_cycle: bool) {
    let store = ObservedStore::default();
    let key = "direct-runtime-history";
    let mut config = CheckpointConfig::with_store(store.clone());
    config.key = Some(key.into());
    let run_config = RunConfig::builder()
        .checkpoint_config(config.clone())
        .max_handoffs(10)
        .session_memory_enabled(false)
        .build();
    let agent = Agent::builder("history-agent")
        .instructions("Answer.")
        .build()
        .unwrap();
    let mut task = AgentTask::new(
        "history-task",
        "history-model",
        PromptBundle::from_instruction_text("Answer.").unwrap(),
        "start",
    );
    task.max_cycles = if cancel_before_fifth_cycle { 5 } else { 4 };
    task.no_tool_policy = NoToolPolicy::Continue;
    let mut response = LLMResponse::new("continue");
    response.token_usage = TokenUsage {
        input_tokens: Some(10),
        output_tokens: Some(5),
        total_tokens: Some(15),
        usage_source: UsageSource::ProviderReported,
        ..TokenUsage::default()
    };
    let workspace = tempfile::tempdir().unwrap();
    let runtime = AgentRuntime::new(ScriptedLlmClient::new(vec![response; 4]))
        .with_default_workspace(workspace.path())
        .with_default_backend("scripted");
    let initial_messages = crate::runtime::engine::build_initial_messages(&task);
    let (run_definition, run_definition_digest) = build_run_definition(RunDefinitionRequest {
        agent: &agent,
        root_input: &task.user_prompt,
        run_config: &run_config,
        resolved: &crate::config::ResolvedModelConfig::new(
            "scripted",
            "history-model",
            "history-model",
            "history-model",
            Vec::new(),
        ),
        model_settings: &ModelSettings::default(),
        task: &task,
        registry: &runtime.tool_registry,
        initial_messages: &initial_messages,
    })
    .unwrap();
    let mut controller = CheckpointResumeController::new(CheckpointControllerRequest {
        config,
        task_id: task.task_id.clone(),
        run_id: "history-run".into(),
        trace_id: "history-trace".into(),
        agent_name: "history-agent".into(),
        run_definition,
        run_definition_digest,
        initial_messages,
        initial_shared_state: Default::default(),
        initial_budget_usage: None,
        extensions: Vec::new(),
        reconciliation_provider: None,
        event_sink: Arc::new(|_| Ok(())),
        event_store: None,
        preloaded_checkpoint: None,
    })
    .unwrap();
    assert!(controller.admit().unwrap().is_none());
    let controller = Arc::new(Mutex::new(controller));
    let token = CancellationToken::default();
    let cancel_token = token.clone();
    let observed_reads = store.history_reads.clone();
    let events = Arc::new(Mutex::new(Vec::<RunEvent>::new()));
    let observed_events = events.clone();
    let result = runtime
        .run_with_controls(
            task,
            RuntimeRunControls {
                checkpoint_controller: Some(CheckpointRuntimeControl::new(controller)),
                cancellation_token: Some(token),
                before_cycle_messages: Some(Arc::new(move |cycle_index, _, _| {
                    assert_eq!(
                        observed_reads.load(Ordering::SeqCst),
                        0,
                        "history loaded during cycle advance"
                    );
                    if cancel_before_fifth_cycle && cycle_index == 5 {
                        cancel_token.cancel();
                    }
                    Vec::new()
                })),
                event_handler: Some(Arc::new(move |event| {
                    observed_events.lock().unwrap().push(event.clone())
                })),
                ..RuntimeRunControls::default()
            },
        )
        .unwrap();

    assert_eq!(
        result
            .cycles
            .iter()
            .map(|cycle| cycle.index)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    assert_eq!(result.token_usage.model_calls.len(), 4);
    assert_eq!(result.token_usage.total_tokens, Some(60));
    assert_eq!(
        store.history_reads.load(Ordering::SeqCst),
        1,
        "hydrate once at public return"
    );
    let stored = store.load_checkpoint(key).unwrap().unwrap();
    assert_eq!(stored.cycles.len(), 1);
    assert!((1..=2).contains(&stored.model_calls.len()));
    assert_eq!(
        stored.history.model_call_count + stored.model_calls.len() as u64,
        4
    );
    // The max-cycle terminal candidate is finalized by Runner; cancellation
    // before cycle five has already committed cycle four.
    assert_eq!(
        stored.history.cycle_count,
        if cancel_before_fifth_cycle { 3 } else { 2 }
    );
    let (status, diagnostic, cycle_index) = if cancel_before_fifth_cycle {
        (AgentStatus::Failed, "run_cancelled", 5)
    } else {
        (AgentStatus::MaxCycles, "run_max_cycles", 4)
    };
    assert_eq!(result.status, status);
    let events = events.lock().unwrap();
    let event = events.iter().find(|event| {
        matches!(event.payload(), RunEventPayload::Diagnostic { code, .. } if code == diagnostic)
    }).expect("terminal diagnostic");
    assert_eq!(event.cycle_index(), Some(cycle_index));
}

#[test]
fn public_runtime_hydrates_history_once_and_reports_logical_max_cycle() {
    run_archived_history(false);
}

#[test]
fn public_runtime_hydrates_cancelled_history_and_reports_logical_cancel_cycle() {
    run_archived_history(true);
}

#[derive(Clone, Default)]
struct ResumeCostMeter(Arc<AtomicBool>);

impl crate::HostCostMeter for ResumeCostMeter {
    fn read(&self) -> Result<Option<crate::HostCost>, String> {
        Ok(Some(crate::HostCost::new(
            "credits",
            if self.0.load(Ordering::SeqCst) { 10 } else { 0 },
        )?))
    }
}

async fn resumed_early_result_keeps_history(direct_runtime: bool, sqlite: bool, budget_mode: u8) {
    let workspace = tempfile::tempdir().unwrap();
    let store = ObservedStore {
        inner: if sqlite {
            Arc::new(
                crate::SqliteCheckpointStore::new(workspace.path().join("resume.sqlite3")).unwrap(),
            )
        } else {
            Arc::new(InMemoryCheckpointStore::new())
        },
        ..ObservedStore::default()
    };
    store.fail_after_fourth_commit.store(true, Ordering::SeqCst);
    let key = "early-result-resume";
    let mut checkpoint_config = CheckpointConfig::with_store(store.clone());
    checkpoint_config.key = Some(key.into());
    checkpoint_config.resume_policy = crate::ResumePolicy::ResumeIfPresent;
    let meter = ResumeCostMeter::default();
    if budget_mode == 2 {
        checkpoint_config.capability_refs.insert(
            "host_cost_meter".into(),
            crate::CapabilityRef::new("resume-cost", "1").unwrap(),
        );
    }
    let token = CancellationToken::default();
    let mut config = RunConfig::builder()
        .checkpoint_config(checkpoint_config.clone())
        .max_cycles(8)
        .no_tool_policy(NoToolPolicy::Continue)
        .cancellation_token(token.clone())
        .session_memory_enabled(false)
        .build();
    if budget_mode > 0 {
        let mut limits = crate::RunBudgetLimits::builder().max_total_tokens(10_000);
        if budget_mode == 2 {
            limits = limits.max_host_cost(crate::HostCost::new("credits", 10).unwrap());
            config.host_cost_meter = Some(Arc::new(meter.clone()));
        }
        config.budget_limits = Some(limits.build().unwrap());
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let steps = (0..8)
        .map(|_| {
            let calls = calls.clone();
            let history_reads = store.history_reads.clone();
            crate::ScriptStep::callback(move |_| {
                assert_eq!(history_reads.load(Ordering::SeqCst), 0);
                let index = calls.fetch_add(1, Ordering::SeqCst) + 1;
                let mut response = LLMResponse::new(format!("cycle {index}"));
                response.token_usage = TokenUsage {
                    input_tokens: Some(10),
                    output_tokens: Some(5),
                    total_tokens: Some(15),
                    usage_source: UsageSource::ProviderReported,
                    ..TokenUsage::default()
                };
                Ok(response)
            })
        })
        .collect();
    let runner = crate::Runner::builder()
        .model_provider(crate::ScriptedModelProvider::from_steps(
            "scripted",
            "history-model",
            steps,
        ))
        .workspace(workspace.path())
        .build()
        .unwrap();
    let agent = Agent::builder("history-agent")
        .instructions("Continue.")
        .model(crate::ModelRef::named("history-model"))
        .build()
        .unwrap();
    let first = runner
        .run_with_config(&agent, "start", config.clone())
        .await;
    assert!(first.is_err(), "expected injected post-commit failure");
    let stored = store.load_checkpoint(key).unwrap().unwrap();
    assert_eq!(stored.cycle_index, 4);
    assert_eq!(
        stored
            .cycles
            .iter()
            .map(|cycle| cycle.index)
            .collect::<Vec<_>>(),
        vec![4]
    );
    assert_eq!(stored.history.cycle_count, 3);
    assert!(stored.terminal_result.is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert_eq!(store.history_reads.load(Ordering::SeqCst), 0);
    if budget_mode == 2 {
        meter.0.store(true, Ordering::SeqCst);
    } else {
        token.cancel();
    }
    let (result, events) = if direct_runtime {
        let task = crate::runtime::run_definition::build_frozen_task(
            &agent,
            &stored,
            &ModelSettings::default(),
        )
        .unwrap();
        let mut controller = CheckpointResumeController::new(CheckpointControllerRequest {
            config: checkpoint_config,
            task_id: stored.task_id.clone(),
            run_id: stored.root_run_id.clone(),
            trace_id: stored.trace_id.clone(),
            agent_name: "history-agent".into(),
            run_definition: stored.run_definition.clone(),
            run_definition_digest: stored.run_definition_digest.clone(),
            initial_messages: Vec::new(),
            initial_shared_state: Default::default(),
            initial_budget_usage: None,
            extensions: Vec::new(),
            reconciliation_provider: None,
            event_sink: Arc::new(|_| Ok(())),
            event_store: None,
            preloaded_checkpoint: None,
        })
        .unwrap();
        assert!(controller.admit().unwrap().is_none());
        let restored = controller.checkpoint().unwrap().clone();
        let controller = Arc::new(Mutex::new(controller));
        let events = Arc::new(Mutex::new(Vec::<RunEvent>::new()));
        let observed = events.clone();
        let result = AgentRuntime::new(ScriptedLlmClient::new(Vec::new()))
            .with_default_workspace(workspace.path())
            .with_default_backend("scripted")
            .run_with_controls(
                task,
                RuntimeRunControls {
                    checkpoint_controller: Some(CheckpointRuntimeControl::new(controller.clone())),
                    initial_messages: Some(restored.messages),
                    initial_cycles: Some(restored.cycles),
                    initial_shared_state: Some(restored.shared_state),
                    initial_model_calls: Some(restored.model_calls),
                    initial_budget_usage: restored.budget_usage,
                    cycle_index_start: Some(5),
                    cycle_count: Some(4),
                    cancellation_token: Some(token),
                    budget_limits: config.budget_limits.clone(),
                    host_cost_meter: config.host_cost_meter.clone(),
                    event_handler: Some(Arc::new(move |event| {
                        observed.lock().unwrap().push(event.clone())
                    })),
                    ..RuntimeRunControls::default()
                },
            )
            .unwrap();
        if budget_mode == 2 {
            assert_early_terminal_rejects_inconsistent_candidates(&controller, &store, &result);
        }
        let events = events.lock().unwrap().clone();
        (result, events)
    } else {
        let result = runner
            .run_with_config(&agent, "start", config.clone())
            .await
            .expect("resume early terminal result");
        let terminal = store.load_checkpoint(key).unwrap().unwrap();
        assert!(terminal.terminal_result.is_some());
        let replay = runner
            .run_with_config(&agent, "start", config)
            .await
            .unwrap();
        assert_eq!(result.result(), replay.result());
        (result.result().clone(), result.events().to_vec())
    };
    assert_eq!(
        result
            .cycles
            .iter()
            .map(|cycle| cycle.index)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    assert_eq!(result.token_usage.model_calls.len(), 4);
    assert_eq!(result.token_usage.total_tokens, Some(60));
    assert_eq!(result.messages, stored.messages);
    assert_eq!(result.shared_state, stored.shared_state);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert_eq!(result.status, AgentStatus::Failed);
    if budget_mode == 2 {
        assert_eq!(
            result.budget_exhaustion.unwrap().enforcement_boundary,
            crate::budget::BudgetEnforcementBoundary::RunStart
        );
    } else {
        let event = events.iter().find(|event| matches!(event.payload(), RunEventPayload::Diagnostic { code, .. } if code == "run_cancelled")).expect("cancellation diagnostic");
        assert_eq!(event.cycle_index(), Some(4));
        if !direct_runtime && budget_mode == 1 {
            let cancelled = events
                .iter()
                .find(|event| matches!(event.payload(), RunEventPayload::RunCancelled { .. }))
                .unwrap();
            let wire = serde_json::to_value(cancelled).unwrap();
            assert!(wire.get("budget_usage").is_some());
            assert!(wire.get("completion_tool_name").is_none());
            assert!(serde_json::from_value::<RunEvent>(wire).is_ok());
        }
    }
}

fn assert_early_terminal_rejects_inconsistent_candidates(
    controller: &Arc<Mutex<CheckpointResumeController>>,
    store: &ObservedStore,
    result: &AgentResult,
) {
    let before = store
        .load_checkpoint("early-result-resume")
        .unwrap()
        .unwrap();
    for mutation in 0..7 {
        let mut candidate = result.clone();
        match mutation {
            0 => {
                candidate.cycles.pop();
            }
            1 => candidate.cycles.clear(),
            2 => {
                candidate.cycles.last_mut().unwrap().assistant_message =
                    "changed committed cycle".into()
            }
            3 => candidate
                .messages
                .push(crate::Message::user("changed messages")),
            4 => {
                candidate
                    .shared_state
                    .insert("changed".into(), serde_json::Value::Bool(true));
            }
            5 => candidate.completion_reason = Some(crate::CompletionReason::Failed),
            6 => {
                candidate
                    .budget_exhaustion
                    .as_mut()
                    .unwrap()
                    .enforcement_boundary = crate::budget::BudgetEnforcementBoundary::CycleStart
            }
            _ => unreachable!(),
        }
        assert!(
            controller
                .lock()
                .unwrap()
                .finalize(candidate, None)
                .is_err(),
            "unsafe terminal mutation {mutation} was accepted"
        );
        assert_eq!(
            store
                .load_checkpoint("early-result-resume")
                .unwrap()
                .unwrap(),
            before
        );
    }
    let mut active = before;
    let arguments = serde_json::json!({});
    let digest = crate::tool_request_digest(
        "pending-effect",
        "write_file",
        &arguments,
        Some("pending-effect-idempotency"),
    )
    .unwrap();
    let mut started = crate::OperationJournalEntry::tool(
        "pending-effect-operation",
        5,
        1,
        digest,
        "pending-effect",
        "write_file",
        arguments.as_object().unwrap().clone(),
        Some("pending-effect-idempotency".into()),
        crate::ToolIdempotency::Unknown,
    );
    started
        .transition_to(crate::OperationState::Started)
        .unwrap();
    active.tool_journal.push(started);
    let token = active.claim_token.clone().unwrap();
    let revision = active.revision;
    assert!(store.progress_checkpoint(active, &token, revision).unwrap());
    let before = store
        .load_checkpoint("early-result-resume")
        .unwrap()
        .unwrap();
    let error = controller
        .lock()
        .unwrap()
        .finalize(result.clone(), None)
        .unwrap_err();
    assert_eq!(error.code(), "checkpoint_terminal_unresolved_operation");
    assert_eq!(
        store
            .load_checkpoint("early-result-resume")
            .unwrap()
            .unwrap(),
        before
    );
}

macro_rules! resume_early_result_tests {
    ($($name:ident, $direct:literal, $mode:literal;)+) => {
        $(#[tokio::test]
        async fn $name() {
            for sqlite in [false, true] {
                resumed_early_result_keeps_history($direct, sqlite, $mode).await;
            }
        })+
    };
}

resume_early_result_tests! {
    public_runtime_resume_early_results_precancelled, true, 0;
    public_runtime_resume_early_results_precancelled_with_budget, true, 1;
    public_runtime_resume_early_results_budget_exhausted, true, 2;
    runner_resume_early_results_precancelled, false, 0;
    runner_resume_early_results_precancelled_with_budget, false, 1;
    runner_resume_early_results_budget_exhausted, false, 2;
}
