use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::mpsc;
use vv_agent::app_server::durable_resume::{
    DurableTurnResumeFuture, DurableTurnResumeOutcome, DurableTurnResumeProvider,
    DurableTurnResumeRequest,
};
use vv_agent::app_server::host::DefaultAppServerHost;
use vv_agent::app_server::outgoing::{OutgoingEnvelope, OutgoingMessageSender};
use vv_agent::app_server::processor::MessageProcessor;
use vv_agent::app_server::protocol::{
    CheckpointSummary, CheckpointSummaryStatus, InterruptionIdempotencySupport,
    InterruptionOperationKind, InterruptionSummary, JsonRpcMessage, JsonRpcNotification,
    JsonRpcRequest, RequestId, ServerNotification, ThreadStartParams, ThreadStatus,
    TurnCompletedParams, TurnResumeResponse, TurnStartResponse, TurnStatus,
};
use vv_agent::app_server::run_adapter::AppServerRunAdapter;
use vv_agent::app_server::thread_state::ThreadStateManager;
use vv_agent::app_server::thread_store::SqliteThreadStore;
use vv_agent::app_server::transport::ConnectionId;
use vv_agent::{
    Agent, CapabilityRef, CheckpointConfig, CheckpointStoreV2, FunctionTool,
    InMemoryCheckpointStoreV2, LLMResponse, ModelRef, NoToolPolicy, ResumePolicy, RunBudgetLimits,
    RunConfig, Runner, ScriptStep, ScriptedModelProvider, TokenUsage, ToolCall, ToolOutput,
    UsageSource,
};

const CONTRACT_SOURCE: &str = include_str!("fixtures/parity/app_server_observable_v1.json");

struct ScriptedDurableResumeProvider {
    outcomes: Mutex<VecDeque<DurableTurnResumeOutcome>>,
    requests: Arc<Mutex<Vec<DurableTurnResumeRequest>>>,
}

impl ScriptedDurableResumeProvider {
    fn new_many(
        outcomes: Vec<DurableTurnResumeOutcome>,
    ) -> (Self, Arc<Mutex<Vec<DurableTurnResumeRequest>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                outcomes: Mutex::new(outcomes.into()),
                requests: requests.clone(),
            },
            requests,
        )
    }
}

struct StableLiveOwnerProvider {
    response: TurnResumeResponse,
    calls: Arc<AtomicUsize>,
}

impl DurableTurnResumeProvider for StableLiveOwnerProvider {
    fn resume_turn(&self, _request: DurableTurnResumeRequest) -> DurableTurnResumeFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let response = self.response.clone();
        Box::pin(async move { Ok(DurableTurnResumeOutcome::ExistingOwner { response }) })
    }
}

impl DurableTurnResumeProvider for ScriptedDurableResumeProvider {
    fn resume_turn(&self, request: DurableTurnResumeRequest) -> DurableTurnResumeFuture {
        self.requests.lock().expect("requests").push(request);
        let outcome = self.outcomes.lock().expect("outcomes").pop_front();
        Box::pin(async move {
            outcome.ok_or_else(|| {
                vv_agent::app_server::protocol::AppServerError::internal(
                    "scripted durable resume outcome already consumed",
                )
            })
        })
    }
}

struct Harness {
    processor: MessageProcessor,
    outgoing: mpsc::Receiver<OutgoingEnvelope>,
    store: SqliteThreadStore,
    thread_id: String,
    turn_id: String,
    requests: Arc<Mutex<Vec<DurableTurnResumeRequest>>>,
}

fn harness(outcome: DurableTurnResumeOutcome) -> Harness {
    harness_with_outcomes(vec![outcome])
}

fn harness_with_outcomes(outcomes: Vec<DurableTurnResumeOutcome>) -> Harness {
    let store = SqliteThreadStore::in_memory().expect("store");
    let thread = store
        .create_thread(ThreadStartParams::default())
        .expect("thread");
    let turn = store
        .create_turn(
            &thread.thread_id,
            vec![json!({"type": "text", "text": "resume"})],
        )
        .expect("turn");
    store
        .set_active_turn(&thread.thread_id, None, ThreadStatus::Idle)
        .expect("idle thread");

    let (runner, agent) = unused_runtime();
    let (provider, requests) = ScriptedDurableResumeProvider::new_many(outcomes);
    let (processor, outgoing) =
        MessageProcessor::new_for_tests_with_runtime_and_durable_resume_provider(
            32,
            runner,
            agent,
            store.clone(),
            Arc::new(provider),
        );
    Harness {
        processor,
        outgoing,
        store,
        thread_id: thread.thread_id,
        turn_id: turn.turn_id,
        requests,
    }
}

fn unused_runtime() -> (Runner, Agent) {
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "unused-model",
            Vec::new(),
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("unused")
        .model(ModelRef::named("unused-model"))
        .build()
        .expect("agent");
    (runner, agent)
}

#[tokio::test]
async fn resumed_run_uses_same_turn_and_emits_reconciliation_sequence() {
    let thread_id = "thread_1";
    let turn_id = "turn_1";
    let run_id = "run-resume-1";
    let checkpoint_key = "tenant-7/run-42";
    let checkpoint = checkpoint(
        checkpoint_key,
        CheckpointSummaryStatus::ReconciliationRequired,
        false,
    );
    let interruption = interruption();
    let response = running_response(thread_id, turn_id, run_id, None);
    let completion = completion(
        thread_id,
        turn_id,
        run_id,
        TurnStatus::Interrupted,
        Some(checkpoint.clone()),
        Some(interruption.clone()),
    );
    let outcome = DurableTurnResumeOutcome::Started {
        response,
        completion: Box::pin(async move { Ok(completion) }),
    };
    let mut harness = harness(outcome);
    initialize(&mut harness).await;

    send_resume(&mut harness, checkpoint_key).await;
    let response = next_message(&mut harness.outgoing).await;
    let JsonRpcMessage::Response(response) = response else {
        panic!("turn/resume response must be first");
    };
    let response: TurnResumeResponse =
        serde_json::from_value(response.result).expect("resume response");
    assert_eq!(response.status, TurnStatus::Running);
    assert_eq!(response.thread_id, harness.thread_id);
    assert_eq!(response.turn_id, harness.turn_id);

    let running = next_notification(&mut harness.outgoing).await;
    let started = next_notification(&mut harness.outgoing).await;
    let idle = next_notification(&mut harness.outgoing).await;
    let completed = next_notification(&mut harness.outgoing).await;
    assert!(matches!(
        running,
        ServerNotification::ThreadStatusChanged(ref params)
            if params.status == ThreadStatus::Running
    ));
    assert!(matches!(
        started,
        ServerNotification::TurnStarted(ref params)
            if params.thread_id == harness.thread_id
                && params.turn_id == harness.turn_id
                && params.run_id.as_deref() == Some(run_id)
                && params.status == Some(TurnStatus::Running)
    ));
    assert!(matches!(
        idle,
        ServerNotification::ThreadStatusChanged(ref params)
            if params.status == ThreadStatus::Idle
    ));
    let ServerNotification::TurnCompleted(completed) = completed else {
        panic!("expected turn/completed");
    };
    assert_eq!(completed.status, TurnStatus::Interrupted);
    assert_eq!(completed.completion_reason, None);
    assert_eq!(completed.error, None);
    assert_eq!(completed.checkpoint, Some(checkpoint));
    assert_eq!(completed.interruption, Some(interruption));

    let stored = harness
        .store
        .get_turn(&harness.thread_id, &harness.turn_id)
        .expect("stored turn")
        .expect("same turn");
    assert_eq!(stored.status, TurnStatus::Interrupted);
    assert_eq!(
        harness
            .store
            .list_turns(&harness.thread_id)
            .expect("turns")
            .len(),
        1
    );
    assert_sensitive_fields_absent(&serde_json::to_value(completed).expect("completion"));
    assert_sensitive_fields_absent(&serde_json::to_value(stored.result).expect("stored result"));
    assert_requests(&harness, checkpoint_key, 1);
}

#[tokio::test]
async fn live_claim_returns_existing_owner_without_notifications_or_new_turn() {
    let checkpoint_key = "tenant-7/run-42";
    let response = running_response(
        "thread_1",
        "turn_1",
        "run-existing-owner",
        Some(checkpoint(
            checkpoint_key,
            CheckpointSummaryStatus::Running,
            false,
        )),
    );
    let outcome = DurableTurnResumeOutcome::ExistingOwner { response };
    let mut harness = harness(outcome);
    initialize(&mut harness).await;

    send_resume(&mut harness, checkpoint_key).await;
    let JsonRpcMessage::Response(response) = next_message(&mut harness.outgoing).await else {
        panic!("expected response");
    };
    let response: TurnResumeResponse =
        serde_json::from_value(response.result).expect("resume response");
    assert_eq!(response.run_id, "run-existing-owner");
    assert_eq!(response.status, TurnStatus::Running);
    let persisted = response.checkpoint.expect("persisted checkpoint summary");
    assert_eq!(persisted.resume_attempt, 2);
    assert_eq!(persisted.status, CheckpointSummaryStatus::Running);
    assert_no_message(&mut harness.outgoing).await;
    assert_eq!(
        harness
            .store
            .list_turns(&harness.thread_id)
            .expect("turns")
            .len(),
        1
    );
    assert_requests(&harness, checkpoint_key, 1);
}

#[tokio::test]
async fn concurrent_live_owner_requests_preserve_persisted_resume_attempt() {
    let store = SqliteThreadStore::in_memory().expect("store");
    let thread = store
        .create_thread(ThreadStartParams::default())
        .expect("thread");
    let turn = store
        .create_turn(&thread.thread_id, Vec::new())
        .expect("turn");
    store
        .set_active_turn(&thread.thread_id, None, ThreadStatus::Idle)
        .expect("idle thread");
    let checkpoint_key = "tenant-7/run-42";
    let response = running_response(
        &thread.thread_id,
        &turn.turn_id,
        "run-existing-owner",
        Some(checkpoint(
            checkpoint_key,
            CheckpointSummaryStatus::Running,
            false,
        )),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn DurableTurnResumeProvider> = Arc::new(StableLiveOwnerProvider {
        response,
        calls: calls.clone(),
    });
    let (runner, agent) = unused_runtime();
    let (mut processor_a, mut outgoing_a) =
        MessageProcessor::new_for_tests_with_runtime_and_durable_resume_provider(
            16,
            runner.clone(),
            agent.clone(),
            store.clone(),
            provider.clone(),
        );
    let (mut processor_b, mut outgoing_b) =
        MessageProcessor::new_for_tests_with_runtime_and_durable_resume_provider(
            16,
            runner,
            agent,
            store.clone(),
            provider,
        );
    let connection_a = ConnectionId::new(11);
    let connection_b = ConnectionId::new(12);
    initialize_processor(&mut processor_a, &mut outgoing_a, connection_a).await;
    initialize_processor(&mut processor_b, &mut outgoing_b, connection_b).await;
    let params = json!({
        "threadId": thread.thread_id,
        "turnId": turn.turn_id,
        "checkpointKey": checkpoint_key,
    });

    tokio::join!(
        processor_a.process_message(connection_a, request(2, "turn/resume", params.clone())),
        processor_b.process_message(connection_b, request(2, "turn/resume", params)),
    );

    for outgoing in [&mut outgoing_a, &mut outgoing_b] {
        let JsonRpcMessage::Response(response) = next_message(outgoing).await else {
            panic!("expected live-owner response");
        };
        assert_eq!(response.result["status"], "running");
        assert_eq!(response.result["checkpoint"]["resumeAttempt"], 2);
        assert_eq!(response.result["checkpoint"]["status"], "running");
        assert_no_message(outgoing).await;
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(store.list_turns(&thread.thread_id).expect("turns").len(), 1);
}

#[tokio::test]
async fn terminal_replay_is_response_only_and_updates_the_existing_turn_snapshot() {
    let checkpoint_key = "tenant-7/run-42";
    let response = TurnResumeResponse {
        thread_id: "thread_1".to_string(),
        turn_id: "turn_1".to_string(),
        run_id: "run-terminal".to_string(),
        status: TurnStatus::Completed,
        final_output: Some("done".to_string()),
        completion_reason: Some("no_tool_finish".to_string()),
        completion_tool_name: None,
        partial_output: None,
        checkpoint: Some(checkpoint(
            checkpoint_key,
            CheckpointSummaryStatus::Completed,
            true,
        )),
        interruption: None,
        error: None,
    };
    let outcomes = vec![
        DurableTurnResumeOutcome::TerminalReplay {
            response: response.clone(),
        },
        DurableTurnResumeOutcome::TerminalReplay { response },
    ];
    let mut harness = harness_with_outcomes(outcomes);
    initialize(&mut harness).await;

    send_resume(&mut harness, checkpoint_key).await;
    let JsonRpcMessage::Response(response) = next_message(&mut harness.outgoing).await else {
        panic!("expected response");
    };
    let response: TurnResumeResponse =
        serde_json::from_value(response.result).expect("resume response");
    assert_eq!(response.status, TurnStatus::Completed);
    assert_eq!(response.final_output.as_deref(), Some("done"));
    assert_eq!(
        response.completion_reason.as_deref(),
        Some("no_tool_finish")
    );
    assert_no_message(&mut harness.outgoing).await;

    let stored = harness
        .store
        .get_turn(&harness.thread_id, &harness.turn_id)
        .expect("stored turn")
        .expect("same turn");
    assert_eq!(stored.status, TurnStatus::Completed);
    assert_eq!(stored.run_id.as_deref(), Some("run-terminal"));
    assert_eq!(stored.result["finalOutput"], "done");
    let completed_at = stored.completed_at;
    assert_eq!(
        harness
            .store
            .list_turns(&harness.thread_id)
            .expect("turns")
            .len(),
        1
    );
    assert_sensitive_fields_absent(&serde_json::to_value(response).expect("response"));
    assert_sensitive_fields_absent(&serde_json::to_value(stored.result).expect("stored result"));

    tokio::time::sleep(Duration::from_millis(5)).await;
    send_resume(&mut harness, checkpoint_key).await;
    let JsonRpcMessage::Response(replayed) = next_message(&mut harness.outgoing).await else {
        panic!("expected repeated terminal response");
    };
    assert_eq!(replayed.result["status"], "completed");
    assert_no_message(&mut harness.outgoing).await;
    let replayed_turn = harness
        .store
        .get_turn(&harness.thread_id, &harness.turn_id)
        .expect("replayed turn")
        .expect("same turn");
    assert_eq!(replayed_turn.completed_at, completed_at);
    assert_eq!(
        harness
            .store
            .list_turns(&harness.thread_id)
            .expect("turns")
            .len(),
        1
    );
    assert_requests(&harness, checkpoint_key, 2);
}

#[tokio::test]
async fn standard_app_server_terminal_replay_uses_real_runner_checkpoint() {
    let checkpoint_key = "app-server/terminal-replay";
    let checkpoint_store = InMemoryCheckpointStoreV2::new();
    let model_calls = Arc::new(AtomicUsize::new(0));
    let calls_for_model = model_calls.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "app-server-checkpoint-model",
        vec![ScriptStep::callback(move |_request| {
            calls_for_model.fetch_add(1, Ordering::SeqCst);
            let mut response = LLMResponse::new("durable app-server answer");
            response.token_usage = TokenUsage {
                prompt_tokens: 7,
                completion_tokens: 3,
                total_tokens: 10,
                input_tokens: 7,
                output_tokens: 3,
                usage_source: UsageSource::ProviderReported,
                ..TokenUsage::default()
            };
            Ok(response)
        })],
    );
    let workspace = tempfile::tempdir().expect("workspace");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(workspace.path())
        .build()
        .expect("runner");
    let agent = Agent::builder("checkpoint-app-server-agent")
        .instructions("Return one durable answer.")
        .model(ModelRef::named("app-server-checkpoint-model"))
        .build()
        .expect("agent");
    let mut checkpoint_config = CheckpointConfig::with_store(checkpoint_store.clone());
    checkpoint_config.key = Some(checkpoint_key.to_string());
    checkpoint_config.resume_policy = ResumePolicy::ResumeIfPresent;
    for (slot, id) in [
        ("approval_provider", "app-server.approval"),
        ("runtime_hook:0", "app-server.steering"),
        ("behavior_affecting_run_metadata", "app-server.run-metadata"),
    ] {
        checkpoint_config.capability_refs.insert(
            slot.to_string(),
            CapabilityRef::new(id, "1").expect("capability ref"),
        );
    }
    let config = RunConfig::builder()
        .max_cycles(1)
        .no_tool_policy(NoToolPolicy::Finish)
        .budget_limits(
            RunBudgetLimits::builder()
                .max_total_tokens(100)
                .build()
                .expect("budget limits"),
        )
        .checkpoint_config(checkpoint_config)
        .build();
    let host = Arc::new(DefaultAppServerHost::from_agent(agent).with_run_config(config));
    let store = SqliteThreadStore::in_memory().expect("thread store");
    let thread = store
        .create_thread(ThreadStartParams::default())
        .expect("thread");
    let (mut processor, mut outgoing) =
        MessageProcessor::with_host(64, runner, host, store.clone());
    let connection = ConnectionId::new(21);
    initialize_processor(&mut processor, &mut outgoing, connection).await;

    processor
        .process_message(
            connection,
            request(
                2,
                "turn/start",
                json!({
                    "threadId": thread.thread_id,
                    "input": [{"type": "text", "text": "run once"}],
                }),
            ),
        )
        .await;
    let JsonRpcMessage::Response(started) = next_message(&mut outgoing).await else {
        panic!("turn/start response");
    };
    let started: TurnStartResponse =
        serde_json::from_value(started.result).expect("turn/start response");
    let first_completion = loop {
        let notification = next_notification(&mut outgoing).await;
        if let ServerNotification::TurnCompleted(completion) = notification {
            break completion;
        }
    };
    assert_eq!(first_completion.status, TurnStatus::Completed);
    assert_eq!(
        first_completion.final_output.as_deref(),
        Some("durable app-server answer")
    );
    assert_eq!(
        first_completion
            .token_usage
            .as_ref()
            .map(|usage| usage.total_tokens),
        Some(10)
    );
    assert_eq!(
        first_completion
            .budget_usage
            .as_ref()
            .and_then(|usage| usage.get("total_tokens")),
        Some(&json!(10))
    );
    let first_checkpoint = first_completion
        .checkpoint
        .as_ref()
        .expect("checkpoint summary");
    assert_eq!(first_checkpoint.key, checkpoint_key);
    assert_eq!(first_checkpoint.status, CheckpointSummaryStatus::Completed);
    assert!(first_checkpoint.terminal_acknowledged);
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);

    let stored_before_replay = store
        .get_turn(&thread.thread_id, &started.turn_id)
        .expect("stored turn")
        .expect("turn");
    assert!(stored_before_replay.result.contains_key("tokenUsage"));
    assert!(stored_before_replay.result.contains_key("budgetUsage"));
    assert!(stored_before_replay.result.contains_key("checkpoint"));
    let completed_at = stored_before_replay.completed_at;

    processor
        .process_message(
            connection,
            request(
                3,
                "turn/resume",
                json!({
                    "threadId": thread.thread_id,
                    "turnId": started.turn_id,
                    "checkpointKey": checkpoint_key,
                }),
            ),
        )
        .await;
    let JsonRpcMessage::Response(replay) = next_message(&mut outgoing).await else {
        panic!("turn/resume response");
    };
    let replay: TurnResumeResponse =
        serde_json::from_value(replay.result).expect("turn/resume response");
    assert_eq!(replay.status, TurnStatus::Completed);
    assert_eq!(
        replay.final_output.as_deref(),
        Some("durable app-server answer")
    );
    assert_eq!(
        replay
            .checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.key.as_str()),
        Some(checkpoint_key)
    );
    assert_no_message(&mut outgoing).await;
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);

    let stored_after_replay = store
        .get_turn(&thread.thread_id, &started.turn_id)
        .expect("replayed turn")
        .expect("same turn");
    assert_eq!(stored_after_replay.completed_at, completed_at);
    assert_eq!(
        stored_after_replay.result.get("tokenUsage"),
        stored_before_replay.result.get("tokenUsage")
    );
    assert_eq!(
        stored_after_replay.result.get("budgetUsage"),
        stored_before_replay.result.get("budgetUsage")
    );
    let terminal = checkpoint_store
        .load_checkpoint_v2(checkpoint_key)
        .expect("checkpoint load")
        .expect("terminal checkpoint");
    assert!(terminal.terminal_acknowledged);
    assert_eq!(terminal.root_run_id, replay.run_id);
}

#[tokio::test]
async fn concurrent_standard_app_servers_have_one_checkpoint_claim_winner() {
    let checkpoint_key = "app-server/concurrent-resume";
    let checkpoint_store = InMemoryCheckpointStoreV2::new();
    let model_calls = Arc::new(AtomicUsize::new(0));
    let first_calls = model_calls.clone();
    let resumed_calls = model_calls.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "app-server-concurrent-model",
        vec![
            ScriptStep::callback(move |_request| {
                first_calls.fetch_add(1, Ordering::SeqCst);
                Ok(LLMResponse::with_tool_calls(
                    "commit cycle one",
                    vec![ToolCall::new(
                        "call-cycle-one",
                        "checkpoint_step",
                        BTreeMap::new(),
                    )],
                ))
            }),
            ScriptStep::callback(move |_request| {
                resumed_calls.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(200));
                Ok(LLMResponse::new("single resumed owner"))
            }),
        ],
    );
    let workspace = tempfile::tempdir().expect("workspace");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(workspace.path())
        .build()
        .expect("runner");
    let step_tool = FunctionTool::builder("checkpoint_step")
        .description("Commit one deterministic cycle before recovery.")
        .handler(|_context, _arguments: Value| async {
            Ok(ToolOutput::text("cycle one committed"))
        })
        .build()
        .expect("step tool");
    let agent = Agent::builder("concurrent-checkpoint-app-server-agent")
        .instructions("Run the checkpoint step, then return the answer.")
        .model(ModelRef::named("app-server-concurrent-model"))
        .tool(step_tool)
        .build()
        .expect("agent");
    let crash_once = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let crash_cycle = crash_once.clone();
    let mut checkpoint_config = CheckpointConfig::with_store(checkpoint_store.clone());
    checkpoint_config.key = Some(checkpoint_key.to_string());
    checkpoint_config.resume_policy = ResumePolicy::ResumeIfPresent;
    for (slot, id) in [
        ("approval_provider", "app-server.approval"),
        ("runtime_hook:0", "app-server.steering"),
        ("behavior_affecting_run_metadata", "app-server.run-metadata"),
        ("before_cycle_messages", "app-server.crash-point"),
    ] {
        checkpoint_config.capability_refs.insert(
            slot.to_string(),
            CapabilityRef::new(id, "1").expect("capability ref"),
        );
    }
    let config = RunConfig::builder()
        .max_cycles(2)
        .no_tool_policy(NoToolPolicy::Finish)
        .checkpoint_config(checkpoint_config)
        .before_cycle_messages(move |cycle, _messages, _state| {
            if cycle == 2 && crash_cycle.swap(false, Ordering::SeqCst) {
                panic!("deterministic App Server crash after cycle one");
            }
            Vec::new()
        })
        .build();
    let host = Arc::new(DefaultAppServerHost::from_agent(agent).with_run_config(config));
    let store = SqliteThreadStore::in_memory().expect("thread store");
    let thread = store
        .create_thread(ThreadStartParams::default())
        .expect("thread");
    let initial_connection = ConnectionId::new(30);
    let (initial_sender, _initial_outgoing) = OutgoingMessageSender::channel(16);
    let initial_state = ThreadStateManager::default();
    let initial_adapter = AppServerRunAdapter::with_host(
        runner.clone(),
        host.clone(),
        store.clone(),
        initial_state.clone(),
        initial_sender,
    );
    let started = initial_adapter
        .start_turn(
            initial_connection,
            vv_agent::app_server::protocol::TurnStartParams {
                thread_id: thread.thread_id.clone(),
                input: vec![json!({"type": "text", "text": "crash after cycle one"})],
                metadata: BTreeMap::new(),
            },
        )
        .await;
    let started = started.expect("App Server start_turn");
    let active = initial_state
        .active_turn(&thread.thread_id)
        .await
        .expect("active turn");
    let crash_error = match active.handle.result().await {
        Ok(_) => panic!("initial run must crash"),
        Err(error) => error,
    };
    assert!(!crash_error.is_empty());
    initial_state
        .clear_active_turn(&thread.thread_id, &started.turn_id)
        .await;
    store
        .set_active_turn(&thread.thread_id, None, ThreadStatus::Idle)
        .expect("reset App Server turn after simulated process crash");
    assert_eq!(model_calls.load(Ordering::SeqCst), 1);
    let mut crashed = checkpoint_store
        .load_checkpoint_v2(checkpoint_key)
        .expect("checkpoint load")
        .expect("crashed checkpoint");
    assert_eq!(crashed.cycle_index, 1);
    assert_eq!(crashed.cycles.len(), 1);
    let original_run_id = crashed.root_run_id.clone();
    crashed.lease_expires_at_ms = Some(1);
    checkpoint_store
        .save_checkpoint_v2(crashed)
        .expect("expire crashed claim");

    let (mut processor_a, mut outgoing_a) =
        MessageProcessor::with_host(64, runner.clone(), host.clone(), store.clone());
    let (mut processor_b, mut outgoing_b) =
        MessageProcessor::with_host(64, runner, host, store.clone());
    let connection_a = ConnectionId::new(31);
    let connection_b = ConnectionId::new(32);
    initialize_processor(&mut processor_a, &mut outgoing_a, connection_a).await;
    initialize_processor(&mut processor_b, &mut outgoing_b, connection_b).await;
    let params = json!({
        "threadId": thread.thread_id,
        "turnId": started.turn_id,
        "checkpointKey": checkpoint_key,
    });
    tokio::join!(
        processor_a.process_message(connection_a, request(3, "turn/resume", params.clone())),
        processor_b.process_message(connection_b, request(3, "turn/resume", params)),
    );

    let JsonRpcMessage::Response(response_a) = next_message(&mut outgoing_a).await else {
        panic!("resume response A");
    };
    let JsonRpcMessage::Response(response_b) = next_message(&mut outgoing_b).await else {
        panic!("resume response B");
    };
    let response_a: TurnResumeResponse =
        serde_json::from_value(response_a.result).expect("resume response A");
    let response_b: TurnResumeResponse =
        serde_json::from_value(response_b.result).expect("resume response B");
    assert_eq!(response_a.run_id, original_run_id);
    assert_eq!(response_b.run_id, original_run_id);
    assert_eq!(response_a.status, TurnStatus::Running);
    assert_eq!(response_b.status, TurnStatus::Running);
    assert_ne!(
        response_a.checkpoint.is_none(),
        response_b.checkpoint.is_none()
    );

    let (winner_outgoing, loser_outgoing) = if response_a.checkpoint.is_none() {
        (&mut outgoing_a, &mut outgoing_b)
    } else {
        (&mut outgoing_b, &mut outgoing_a)
    };
    let completion = loop {
        let notification = next_notification(winner_outgoing).await;
        if let ServerNotification::TurnCompleted(completion) = notification {
            break completion;
        }
    };
    assert_no_message(loser_outgoing).await;
    assert_eq!(completion.status, TurnStatus::Completed);
    assert_eq!(
        completion.final_output.as_deref(),
        Some("single resumed owner")
    );
    assert_eq!(completion.run_id.as_deref(), Some(original_run_id.as_str()));
    let summary = completion.checkpoint.expect("terminal checkpoint summary");
    assert_eq!(summary.resume_attempt, 2);
    assert!(summary.terminal_acknowledged);
    assert_eq!(model_calls.load(Ordering::SeqCst), 2);
    assert_eq!(store.list_turns(&thread.thread_id).expect("turns").len(), 1);
}

include!("app_server_turn_resume/helpers.rs");
