use super::*;
use vv_agent::llm::ScriptStep;
use vv_agent::runtime::backends::distributed::toolset_schema_digest;
use vv_agent::{
    build_default_registry, CheckpointStatus, ControllerCommand, ControllerCommandVariant,
    ControllerHandle, CycleDispatchResult, HostInteractionMessage, HostInteractionRequest,
    SqliteCheckpointStore, ToolCall, ToolCallOutcome, ToolError, ToolExecutor, ToolFuture,
    ToolRunContext, ToolSpec, ToolSpecContext, ToolsetRef,
};

struct ChoiceTool(Arc<AtomicUsize>, Option<InMemoryCheckpointStore>);

#[path = "host_interaction/race_store.rs"]
mod race_store;

#[test]
#[ignore = "requires a real cross-language host-interaction checkpoint"]
fn cross_language_host_interaction_store() {
    let directory = std::env::var("VV_AGENT_CROSS_HOST_DIR").unwrap();
    let mode = std::env::var("VV_AGENT_CROSS_HOST_MODE").unwrap();
    assert!(matches!(mode.as_str(), "respond" | "read"));
    let store: Arc<dyn CheckpointStore> = match std::env::var("VV_AGENT_CROSS_HOST_REDIS_URL") {
        Ok(url) => Arc::new(vv_agent::RedisCheckpointStore::new(&url).unwrap()),
        Err(_) => Arc::new(
            SqliteCheckpointStore::new(std::path::Path::new(&directory).join("host-tool.sqlite"))
                .unwrap(),
        ),
    };
    let checkpoint = store.load_checkpoint("host-tool").unwrap().unwrap();
    let text = "Europe 中文 https://example.invalid/?keep=1";
    assert_eq!(checkpoint.cycle_index, 1);
    assert_eq!(checkpoint.model_calls.len(), 1);
    assert_eq!(
        checkpoint.cycles[0].tool_results[0].content,
        "Region choice requested."
    );
    assert_eq!(
        checkpoint.cycles[0].tool_results[1].error_code.as_deref(),
        Some("skipped_due_to_host_interaction")
    );
    assert!(checkpoint.tool_journal.is_empty() && checkpoint.model_call_journal.is_empty());
    if mode == "respond" {
        let request = checkpoint.active_host_interaction.as_ref().unwrap();
        assert_eq!(
            request.prompt,
            "Choose a region: https://example.invalid/?keep=1"
        );
        let command = ControllerCommand::new(
            "cross-response",
            ControllerHandle::new("host-tool", &checkpoint.root_run_id, &checkpoint.trace_id)
                .unwrap(),
            checkpoint.resume_attempt,
            checkpoint.revision,
            ControllerCommandVariant::HostInteractionResponse {
                interaction_id: request.interaction_id.clone(),
                logical_cycle: request.logical_cycle,
                operation_id: request.operation_id.clone(),
                tool_call_id: request.tool_call_id.clone(),
                request_digest: request.request_digest.clone(),
                response: HostInteractionMessage::user(text).unwrap(),
            },
        )
        .unwrap();
        assert!(matches!(
            store.resolve_controller_command(command.clone()).unwrap(),
            vv_agent::ControllerCommandResolution::Applied { .. }
        ));
        let resolved = store.load_checkpoint("host-tool").unwrap().unwrap();
        assert!(matches!(
            store.resolve_controller_command(command.clone()).unwrap(),
            vv_agent::ControllerCommandResolution::Replayed { .. }
        ));
        assert_eq!(
            store.load_checkpoint("host-tool").unwrap().unwrap(),
            resolved
        );
        let record = store
            .find_resolved_pending_host_interaction("host-tool")
            .unwrap()
            .unwrap();
        let recovery = vv_agent::HostInteractionRecoveryEnvelope {
            schema_version: "vv-agent.host-interaction-recovery.v1".into(),
            record_id: record.record_id,
            checkpoint_key: "host-tool".into(),
            run_id: checkpoint.root_run_id,
            trace_id: checkpoint.trace_id,
            claim_mode: "recovery".into(),
            resume_attempt: resolved.resume_attempt,
            expected_revision: resolved.revision,
            logical_cycle: request.logical_cycle,
            interaction_id: request.interaction_id.clone(),
            operation_id: request.operation_id.clone(),
            tool_call_id: request.tool_call_id.clone(),
            request_digest: request.request_digest.clone(),
            command_id: command.command_id,
        };
        assert_eq!(
            store
                .claim_and_consume_host_interaction_response(recovery.clone())
                .unwrap()
                .kind,
            "applied"
        );
        let consumed = store.load_checkpoint("host-tool").unwrap();
        assert_eq!(
            store
                .claim_and_consume_host_interaction_response(recovery)
                .unwrap()
                .kind,
            "replayed"
        );
        assert_eq!(store.load_checkpoint("host-tool").unwrap(), consumed);
    }
    let checkpoint = store.load_checkpoint("host-tool").unwrap().unwrap();
    assert_eq!(checkpoint.status, CheckpointStatus::Running);
    assert_eq!(checkpoint.claimed_cycle, Some(2));
    assert!(checkpoint.claim_token.is_some() && checkpoint.terminal_result.is_none());
    assert_eq!(
        checkpoint
            .messages
            .iter()
            .filter(|message| message.content == text)
            .count(),
        1
    );
}

fn spawn_host_worker(directory: &std::path::Path, phase: &str) -> std::process::Child {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "host_interaction::native_host_tool_commits_cycle_and_resumes_after_sqlite_reopen",
            "--nocapture",
        ])
        .env("VV_HOST_TEST_DIR", directory)
        .env("VV_HOST_TEST_PHASE", phase)
        .spawn()
        .unwrap()
}

fn wait_host_worker(child: &mut std::process::Child) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("host interaction worker did not finish");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn native_host_concurrent_recovery_retains_exclusive_claim() {
    let directory = tempfile::tempdir().unwrap();
    let mut owner = spawn_host_worker(directory.path(), "concurrent_owner");
    let verification = std::panic::catch_unwind(|| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !directory.path().join("model-entered").exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "worker did not enter the model"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let store = SqliteCheckpointStore::new(directory.path().join("host-tool.sqlite")).unwrap();
        let before = store.load_checkpoint("host-tool").unwrap().unwrap();
        assert!(before.claim_token.is_some());
        assert_eq!(
            before.model_call_journal[0].state,
            vv_agent::OperationState::Started
        );
        let mut duplicate = spawn_host_worker(directory.path(), "concurrent_duplicate");
        assert!(wait_host_worker(&mut duplicate).success());
        assert_eq!(store.load_checkpoint("host-tool").unwrap().unwrap(), before);
    });
    std::fs::write(directory.path().join("release-model"), b"ready").unwrap();
    let status = wait_host_worker(&mut owner);
    verification.unwrap();
    assert!(status.success());
    let store = SqliteCheckpointStore::new(directory.path().join("host-tool.sqlite")).unwrap();
    let terminal = store.load_checkpoint("host-tool").unwrap().unwrap();
    assert!(terminal.terminal_acknowledged);
    assert_eq!(terminal.model_calls.len(), 2);
}

#[test]
fn native_host_unclaimed_recovery_races_before_wake_cas() {
    assert_unclaimed_recovery_race(false);
}

#[test]
fn native_host_unclaimed_recovery_observes_completed_wake() {
    assert_unclaimed_recovery_race(true);
}

fn assert_unclaimed_recovery_race(after_wake: bool) {
    let directory = tempfile::tempdir().unwrap();
    if after_wake {
        std::fs::write(directory.path().join("race-after-wake"), b"ready").unwrap();
    }
    let mut producer = spawn_host_worker(directory.path(), "prepare_recovery");
    assert!(wait_host_worker(&mut producer).success());
    let store = SqliteCheckpointStore::new(directory.path().join("host-tool.sqlite")).unwrap();
    let before = store.load_checkpoint("host-tool").unwrap().unwrap();
    assert!(before.claim_token.is_none());
    let mut left = spawn_host_worker(directory.path(), "race_left");
    let mut right = spawn_host_worker(directory.path(), "race_right");
    let verification = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        race_store::wait_for(&directory.path().join("ready-race_left"));
        race_store::wait_for(&directory.path().join("ready-race_right"));
        assert_eq!(store.load_checkpoint("host-tool").unwrap().unwrap(), before);
        std::fs::write(directory.path().join("release-claim"), b"ready").unwrap();
        let admitted = if after_wake {
            race_store::wait_for(&directory.path().join("model-entered"));
            store.load_checkpoint("host-tool").unwrap().unwrap()
        } else {
            race_store::wait_for(&directory.path().join("admitted"));
            vv_agent::runtime::checkpoint_codec::checkpoint_from_value(
                &serde_json::from_slice(
                    &std::fs::read(directory.path().join("admitted.json")).unwrap(),
                )
                .unwrap(),
                vv_agent::checkpoint::MAX_WIRE_INTEGER,
            )
            .unwrap()
        };
        std::fs::write(directory.path().join("release-loser-claim"), b"ready").unwrap();
        race_store::wait_for(&directory.path().join("loser-pending"));
        assert_eq!(
            store.load_checkpoint("host-tool").unwrap().unwrap(),
            admitted
        );
        if !after_wake {
            assert_eq!(admitted.revision, before.revision + 1);
        }
        assert_eq!(admitted.resume_attempt, before.resume_attempt + 1);
        assert_eq!(admitted.claimed_cycle, Some(2));
        assert_eq!(admitted.model_calls.len(), 1);
    }));
    std::fs::write(directory.path().join("release-claim"), b"ready").unwrap();
    std::fs::write(directory.path().join("release-owner"), b"ready").unwrap();
    std::fs::write(directory.path().join("release-loser-claim"), b"ready").unwrap();
    std::fs::write(directory.path().join("release-model"), b"ready").unwrap();
    let statuses = [wait_host_worker(&mut left), wait_host_worker(&mut right)];
    verification.unwrap();
    assert!(statuses.iter().all(std::process::ExitStatus::success));
    let terminal = store.load_checkpoint("host-tool").unwrap().unwrap();
    assert!(terminal.terminal_acknowledged && terminal.claim_token.is_none());
    assert_eq!(terminal.model_calls.len(), 2);
}

#[test]
fn native_host_process_exit_replays_in_a_new_worker() {
    for fault in ["admission", "response", "model_receipt"] {
        let directory = tempfile::tempdir().unwrap();
        for (phase, exit_code) in [
            (format!("crash_{fault}"), 23),
            (format!("replay_{fault}"), 0),
        ] {
            let mut child = spawn_host_worker(directory.path(), &phase);
            let status = wait_host_worker(&mut child);
            assert_eq!(status.code(), Some(exit_code));
            let store =
                SqliteCheckpointStore::new(directory.path().join("host-tool.sqlite")).unwrap();
            let checkpoint = store.load_checkpoint("host-tool").unwrap().unwrap();
            if phase == "crash_admission" {
                assert_eq!(checkpoint.status, CheckpointStatus::HostInteraction);
                assert_eq!(checkpoint.model_calls.len(), 1);
                assert!(checkpoint.claim_token.is_none());
            } else if phase == "crash_response" {
                assert_eq!(checkpoint.status, CheckpointStatus::Running);
                assert!(checkpoint.claim_token.is_some());
                assert!(checkpoint.model_call_journal.is_empty());
                assert_eq!(checkpoint.model_calls.len(), 1);
            } else if phase == "crash_model_receipt" {
                assert_eq!(checkpoint.status, CheckpointStatus::Running);
                assert!(checkpoint.claim_token.is_some());
                assert_eq!(checkpoint.model_calls.len(), 2);
                assert_eq!(
                    checkpoint.model_call_journal[0].state,
                    vv_agent::OperationState::Succeeded
                );
            } else {
                assert!(checkpoint.terminal_acknowledged);
                assert_eq!(checkpoint.model_calls.len(), 2);
            }
        }
    }
}

#[tokio::test]
async fn local_host_tool_returns_nonterminal_result_without_finalizing() {
    local_host_tool_result(false).await;
}

#[tokio::test]
async fn local_cancel_before_host_interaction_keeps_result_without_waiting() {
    local_host_tool_result(true).await;
}

async fn local_host_tool_result(cancel: bool) {
    let store = InMemoryCheckpointStore::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut tools = build_default_registry();
    tools
        .register_executor(Arc::new(ChoiceTool(
            calls.clone(),
            cancel.then(|| store.clone()),
        )))
        .unwrap();
    let mut checkpoint = CheckpointConfig::with_store(store.clone());
    checkpoint.key = Some("local-host".into());
    checkpoint.capability_refs.insert(
        "session".into(),
        CapabilityRef::new("session.local-host", "1").unwrap(),
    );
    checkpoint.capability_refs.insert(
        "tool_registry_factory".into(),
        CapabilityRef::new("tools.local-host", "1").unwrap(),
    );
    let session = MemorySession::new("local-host");
    let config = RunConfig::builder()
        .max_cycles(2)
        .session(session.clone())
        .checkpoint_config(checkpoint)
        .tool_registry_factory(move || tools.clone())
        .build();
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "driver-model",
            vec![LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::new(
                    "choice",
                    "request_choice",
                    Default::default(),
                )],
            )],
        ))
        .workspace(".")
        .build()
        .unwrap();
    let agent = Agent::builder("region")
        .instructions("Choose a region")
        .model(ModelRef::named("driver-model"))
        .build()
        .unwrap();
    let result = runner
        .run_with_config(&agent, "choose", config)
        .await
        .unwrap();
    if cancel {
        assert_eq!(result.status(), AgentStatus::Failed);
        assert_eq!(
            result.result().completion_reason,
            Some(vv_agent::CompletionReason::Cancelled)
        );
        let terminal = store.load_checkpoint("local-host").unwrap().unwrap();
        assert!(terminal.terminal_result.is_some() && terminal.claim_token.is_none());
        assert!(terminal.active_host_interaction.is_none());
        assert!(terminal
            .cycles
            .iter()
            .flat_map(|cycle| &cycle.tool_results)
            .any(|result| result.content == "Region choice requested."));
        assert!(!terminal
            .event_outbox
            .iter()
            .any(|event| event.event["type"] == "host_interaction_requested"));
        return;
    }
    assert_eq!(result.status(), AgentStatus::HostInteraction);
    assert_eq!(result.final_output(), None);
    assert_eq!(
        result.result().wait_reason.as_deref(),
        Some("host_interaction")
    );
    assert_eq!(
        vv_agent::AgentResult::from_dict(&result.result().to_dict()).unwrap(),
        *result.result()
    );
    let waiting = store.load_checkpoint("local-host").unwrap().unwrap();
    assert_eq!(waiting.status, CheckpointStatus::HostInteraction);
    assert!(waiting.terminal_result.is_none() && waiting.claim_token.is_none());
    assert!(session.get_items(None).await.unwrap().is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

impl ToolExecutor for ChoiceTool {
    fn name(&self) -> &str {
        "request_choice"
    }
    fn description(&self) -> &str {
        "Request a region choice."
    }
    fn spec(&self, _: &ToolSpecContext) -> Result<ToolSpec, ToolError> {
        Ok(ToolSpec::new(
            self.name(),
            self.description(),
            Arc::new(|_, _| panic!("runtime must retain the typed executor")),
        ))
    }
    fn run<'a>(
        &'a self,
        _: ToolCall,
        _: ToolRunContext<'a>,
    ) -> ToolFuture<'a, ToolExecutionResult> {
        Box::pin(async { panic!("runtime must use the typed outcome") })
    }
    fn run_outcome<'a>(
        &'a self,
        call: ToolCall,
        ctx: ToolRunContext<'a>,
    ) -> ToolFuture<'a, ToolCallOutcome> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            if let Some(store) = &self.1 {
                let checkpoint = store
                    .load_checkpoint(ctx.context.checkpoint_key.as_ref().unwrap())
                    .unwrap()
                    .unwrap();
                let command = ControllerCommand::new(
                    "cancel-before-choice",
                    ControllerHandle::new(
                        &checkpoint.checkpoint_key,
                        &checkpoint.root_run_id,
                        &checkpoint.trace_id,
                    )
                    .unwrap(),
                    checkpoint.resume_attempt,
                    checkpoint.revision,
                    ControllerCommandVariant::Cancel,
                )
                .unwrap();
                assert!(matches!(
                    store.resolve_controller_command(command).unwrap(),
                    vv_agent::ControllerCommandResolution::Applied { .. }
                ));
            }
            let request = HostInteractionRequest::new(
                "region-choice",
                u64::from(ctx.context.cycle_index),
                ctx.context
                    .operation_id
                    .as_ref()
                    .expect("durable tool plan"),
                &call.id,
                "Choose a region: https://example.invalid/?keep=1",
            )
            .unwrap();
            Ok(ToolCallOutcome::HostInteraction {
                result: ToolExecutionResult::success(call.id, "Region choice requested."),
                request,
            })
        })
    }
}

#[tokio::test]
async fn native_host_tool_commits_cycle_and_resumes_after_sqlite_reopen() {
    let external_directory = std::env::var_os("VV_HOST_TEST_DIR").map(std::path::PathBuf::from);
    let directory = external_directory
        .is_none()
        .then(|| tempfile::tempdir().unwrap());
    let workspace =
        external_directory.unwrap_or_else(|| directory.as_ref().unwrap().path().to_path_buf());
    let phase = std::env::var("VV_HOST_TEST_PHASE").unwrap_or_default();
    let race_worker = matches!(phase.as_str(), "race_left" | "race_right");
    let prepare_recovery = phase == "prepare_recovery";
    let participant = phase.clone();
    let concurrent_owner =
        phase == "concurrent_owner" || (race_worker && workspace.join("race-after-wake").exists());
    let concurrent_duplicate = phase == "concurrent_duplicate";
    let exchange_write = phase == "exchange_write";
    let replaying_admission = phase == "replay_admission";
    let replaying_model = phase == "replay_model_receipt";
    let replaying_response = phase == "replay_response" || replaying_model;
    let replaying = replaying_admission || replaying_response || race_worker;
    let path = workspace.join("host-tool.sqlite");
    let mut store: Arc<dyn CheckpointStore> = match std::env::var("VV_AGENT_CROSS_HOST_REDIS_URL") {
        Ok(url) => {
            assert!(exchange_write);
            Arc::new(vv_agent::RedisCheckpointStore::new(&url).unwrap())
        }
        Err(_) => Arc::new(SqliteCheckpointStore::new(&path).unwrap()),
    };
    let tools_called = Arc::new(AtomicUsize::new(0));
    let models_called = Arc::new(AtomicUsize::new(0));
    let mut tools = build_default_registry();
    tools
        .register_executor(Arc::new(ChoiceTool(tools_called.clone(), None)))
        .unwrap();
    tools
        .register_tool(
            "later",
            "Run after the choice",
            Arc::new(|_, _| panic!("later must be skipped")),
        )
        .unwrap();
    let checkpoint_ref = CapabilityRef::new("checkpoint.host-tool", "1").unwrap();
    let llm_ref = CapabilityRef::new("llm.host-tool", "1").unwrap();
    let registry = DistributedCapabilityRegistry::new();
    registry.register_checkpoint_store(checkpoint_ref.clone(), store.clone());
    let event_ref = CapabilityRef::new("events.host-tool", "1").unwrap();
    let event_store = store.clone();
    let observed_tools = tools_called.clone();
    let observed_models = models_called.clone();
    registry.register_event_sink(
        event_ref.clone(),
        Arc::new(move |event| {
            if phase == "crash_admission"
                && matches!(
                    &event.payload,
                    vv_agent::RunEventPayload::HostInteractionRequested { .. }
                )
            {
                let checkpoint = event_store.load_checkpoint("host-tool").unwrap().unwrap();
                assert_eq!(checkpoint.status, CheckpointStatus::HostInteraction);
                assert!(checkpoint.claim_token.is_none());
                assert_eq!(observed_models.load(Ordering::SeqCst), 1);
                assert_eq!(observed_tools.load(Ordering::SeqCst), 1);
                std::process::exit(23);
            }
            let response_exit = phase == "crash_response"
                && matches!(&event.payload, vv_agent::RunEventPayload::CycleStarted);
            let model_exit = phase == "crash_model_receipt"
                && matches!(
                    &event.payload,
                    vv_agent::RunEventPayload::ModelCallCompleted { .. }
                );
            if event.cycle_index == Some(2) && (response_exit || model_exit) {
                let checkpoint = event_store.load_checkpoint("host-tool").unwrap().unwrap();
                if response_exit {
                    assert!(checkpoint.model_call_journal.is_empty());
                } else {
                    assert_eq!(
                        checkpoint.model_call_journal[0].state,
                        vv_agent::OperationState::Succeeded
                    );
                }
                assert_eq!(
                    checkpoint
                        .messages
                        .iter()
                        .filter(
                            |message| message.content == "Europe https://example.invalid/?keep=1"
                        )
                        .count(),
                    1
                );
                assert_eq!(
                    observed_models.load(Ordering::SeqCst),
                    if model_exit { 2 } else { 1 }
                );
                assert_eq!(observed_tools.load(Ordering::SeqCst), 1);
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64;
                assert!(matches!(
                    event_store
                        .renew_checkpoint_claim(
                            "host-tool",
                            checkpoint.claim_token.as_deref().unwrap(),
                            now + 1,
                            now
                        )
                        .unwrap(),
                    vv_agent::CheckpointRenewalOutcome::Renewed { .. }
                ));
                std::process::exit(23);
            }
        }),
    );
    let first_calls = models_called.clone();
    registry.register_llm_client(
        llm_ref.clone(),
        Arc::new(ScriptedLlmClient::from_steps(vec![ScriptStep::callback(
            move |_| {
                first_calls.fetch_add(1, Ordering::SeqCst);
                Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![
                        ToolCall::new("choice", "request_choice", Default::default()),
                        ToolCall::new("later", "later", Default::default()),
                    ],
                ))
            },
        )])),
    );
    let toolset = ToolsetRef {
        id: "host-tools".into(),
        version: "1".into(),
        schema_digest: toolset_schema_digest(&tools).unwrap(),
    };
    registry
        .register_toolset(toolset.clone(), tools.clone())
        .unwrap();
    let mut recipe =
        RuntimeRecipe::new("", "scripted", "driver-model", workspace.to_str().unwrap());
    recipe.capabilities = DistributedCapabilities {
        checkpoint_store_ref: Some(checkpoint_ref.clone()),
        llm_client_ref: Some(llm_ref.clone()),
        toolset_ref: toolset,
        event_sink_ref: Some(event_ref.clone()),
        ..Default::default()
    };
    let enqueuer = Arc::new(RecordingEnqueuer::default());
    let backend = DistributedBackend::nonblocking(recipe, registry.clone(), enqueuer.clone());
    let worker = DistributedCycleWorker::new(registry.clone());
    if concurrent_duplicate {
        let payload =
            serde_json::from_slice(&std::fs::read(workspace.join("recovery.json")).unwrap())
                .unwrap();
        let envelope = DistributedRunEnvelope::from_dict(&payload).unwrap();
        let before = store.load_checkpoint("host-tool").unwrap();
        assert_eq!(
            worker.run_cycle(envelope).unwrap(),
            CycleDispatchResult::pending()
        );
        assert_eq!(store.load_checkpoint("host-tool").unwrap(), before);
        assert_eq!(tools_called.load(Ordering::SeqCst), 0);
        assert_eq!(models_called.load(Ordering::SeqCst), 0);
        return;
    }
    let mut checkpoint = CheckpointConfig {
        store: Some(store.clone()),
        ..Default::default()
    };
    checkpoint.key = Some("host-tool".into());
    checkpoint.resume_policy = ResumePolicy::New;
    checkpoint
        .capability_refs
        .insert("checkpoint_store".into(), checkpoint_ref.clone());
    checkpoint.capability_refs.insert(
        "tool_registry_factory".into(),
        CapabilityRef::new("tools.host-tool", "1").unwrap(),
    );
    checkpoint
        .capability_refs
        .insert("event_sink".into(), event_ref);
    let mut config = RunConfig::builder()
        .max_cycles(3)
        .checkpoint_config(checkpoint)
        .tool_registry_factory(move || tools.clone())
        .execution_mode(ExecutionMode::Distributed(backend.clone()))
        .build();
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "driver-model",
            Vec::new(),
        ))
        .workspace(&workspace)
        .build()
        .unwrap();
    let agent = Agent::builder("region")
        .instructions("Ask before continuing.")
        .model(ModelRef::named("driver-model"))
        .build()
        .unwrap();
    let (handle, first) = if replaying {
        let payload =
            serde_json::from_slice(&std::fs::read(workspace.join("envelope.json")).unwrap())
                .unwrap();
        let envelope = DistributedRunEnvelope::from_dict(&payload).unwrap();
        (
            vv_agent::DistributedRunHandle {
                checkpoint_key: envelope.checkpoint_config.key.clone(),
                run_id: envelope.root_run_id.clone(),
                trace_id: envelope.trace_id.clone(),
            },
            envelope,
        )
    } else {
        let handle = runner
            .start_distributed(&agent, "Choose a region.", config.clone())
            .await
            .unwrap();
        let envelope = enqueuer.take_one();
        std::fs::write(
            workspace.join("envelope.json"),
            serde_json::to_vec(&envelope.to_dict()).unwrap(),
        )
        .unwrap();
        (handle, envelope)
    };
    let response_text = "Europe https://example.invalid/?keep=1";
    if !replaying_response && !race_worker {
        assert_eq!(
            worker.run_cycle(first.clone()).unwrap(),
            CycleDispatchResult::pending()
        );
        let waiting = store
            .load_checkpoint(&handle.checkpoint_key)
            .unwrap()
            .unwrap();
        assert_eq!(waiting.status, CheckpointStatus::HostInteraction);
        assert!(waiting.claim_token.is_none());
        assert!(waiting.tool_journal.is_empty() && waiting.model_call_journal.is_empty());
        assert_eq!(waiting.cycle_index, 1);
        assert_eq!(
            waiting.cycles[0].tool_results[0].content,
            "Region choice requested."
        );
        assert_eq!(
            waiting.cycles[0].tool_results[1].error_code.as_deref(),
            Some("skipped_due_to_host_interaction")
        );
        assert_eq!(tools_called.load(Ordering::SeqCst), usize::from(!replaying));
        if exchange_write {
            assert_eq!(models_called.load(Ordering::SeqCst), 1);
            return;
        }
        let request = waiting.active_host_interaction.as_ref().unwrap();
        let command = ControllerCommand::new(
            "region-response",
            ControllerHandle::new(&handle.checkpoint_key, &handle.run_id, &handle.trace_id)
                .unwrap(),
            waiting.resume_attempt,
            waiting.revision,
            ControllerCommandVariant::HostInteractionResponse {
                interaction_id: request.interaction_id.clone(),
                logical_cycle: request.logical_cycle,
                operation_id: request.operation_id.clone(),
                tool_call_id: request.tool_call_id.clone(),
                request_digest: request.request_digest.clone(),
                response: HostInteractionMessage::user(response_text).unwrap(),
            },
        )
        .unwrap();
        store.resolve_controller_command(command.clone()).unwrap();
        let resolved = store.load_checkpoint(&handle.checkpoint_key).unwrap();
        store.resolve_controller_command(command).unwrap();
        assert_eq!(
            store.load_checkpoint(&handle.checkpoint_key).unwrap(),
            resolved
        );
    }
    let reopened_store = Arc::new(SqliteCheckpointStore::new(&path).unwrap());
    store = reopened_store.clone();
    registry.register_checkpoint_store(checkpoint_ref.clone(), store.clone());
    config.checkpoint_config.as_mut().unwrap().store = Some(store.clone());
    let second_calls = models_called.clone();
    let model_workspace = workspace.clone();
    registry.register_llm_client(
        llm_ref,
        Arc::new(ScriptedLlmClient::from_steps(vec![ScriptStep::callback(
            move |request| {
                second_calls.fetch_add(1, Ordering::SeqCst);
                assert_eq!(
                    request
                        .messages
                        .iter()
                        .filter(|message| message.content == response_text)
                        .count(),
                    1
                );
                assert!(request
                    .messages
                    .iter()
                    .any(|message| message.content == "Region choice requested."));
                if concurrent_owner {
                    std::fs::write(model_workspace.join("model-entered"), b"ready").unwrap();
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
                    while !model_workspace.join("release-model").exists() {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "duplicate did not release the model"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                }
                Ok(LLMResponse::new("Europe selected."))
            },
        )])),
    );
    let recovery = if race_worker {
        let payload =
            serde_json::from_slice(&std::fs::read(workspace.join("recovery.json")).unwrap())
                .unwrap();
        registry.register_checkpoint_store(
            checkpoint_ref,
            Arc::new(race_store::RaceStore {
                inner: reopened_store,
                participant,
            }),
        );
        DistributedRunEnvelope::from_dict(&payload).unwrap()
    } else {
        let previous = if replaying_response {
            let payload =
                serde_json::from_slice(&std::fs::read(workspace.join("recovery.json")).unwrap())
                    .unwrap();
            let previous = DistributedRunEnvelope::from_dict(&payload).unwrap();
            let before = store.load_checkpoint(&handle.checkpoint_key).unwrap();
            assert!(worker
                .run_cycle(previous.clone())
                .unwrap_err()
                .contains("resume_attempt"));
            assert_eq!(
                store.load_checkpoint(&handle.checkpoint_key).unwrap(),
                before
            );
            previous
        } else {
            first
        };
        assert!(matches!(
            backend
                .advance(
                    &previous,
                    if replaying_response {
                        DistributedDeliveryOutcome::TransportFailure(
                            "worker exited after response consumption".into(),
                        )
                    } else {
                        DistributedDeliveryOutcome::worker(CycleDispatchResult::pending())
                    }
                )
                .unwrap(),
            DistributedAdvanceDecision::Dispatch { .. }
        ));
        let recovery = enqueuer.take_one();
        std::fs::write(
            workspace.join("recovery.json"),
            serde_json::to_vec(&recovery.to_dict()).unwrap(),
        )
        .unwrap();
        if prepare_recovery {
            assert_eq!(models_called.load(Ordering::SeqCst), 1);
            assert_eq!(tools_called.load(Ordering::SeqCst), 1);
            return;
        }
        recovery
    };
    let response = worker.run_cycle(recovery.clone()).unwrap();
    if race_worker && response == CycleDispatchResult::pending() {
        assert_eq!(models_called.load(Ordering::SeqCst), 0);
        assert_eq!(tools_called.load(Ordering::SeqCst), 0);
        std::fs::write(workspace.join("loser-pending"), b"ready").unwrap();
        return;
    }
    let CycleDispatchResult::TerminalCandidate { result, .. } = &response else {
        panic!("expected completed candidate: {response:?}");
    };
    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(result.final_answer.as_deref(), Some("Europe selected."));
    let owned = store.load_checkpoint(&handle.checkpoint_key).unwrap();
    assert_eq!(
        result.token_usage.model_calls,
        owned.as_ref().unwrap().model_calls
    );
    assert_eq!(
        worker.run_cycle(recovery.clone()).unwrap(),
        CycleDispatchResult::pending()
    );
    assert_eq!(
        store.load_checkpoint(&handle.checkpoint_key).unwrap(),
        owned
    );
    let finish = backend
        .advance(&recovery, DistributedDeliveryOutcome::worker(response))
        .unwrap();
    let completed = runner
        .finalize_distributed(&agent, "Choose a region.", finish.clone(), config.clone())
        .await
        .unwrap();
    assert_eq!(completed.status(), AgentStatus::Completed);
    assert_eq!(completed.final_output(), Some("Europe selected."));
    let terminal = store
        .load_checkpoint(&handle.checkpoint_key)
        .unwrap()
        .unwrap();
    assert!(terminal.terminal_acknowledged && terminal.claim_token.is_none());
    let replay = runner
        .finalize_distributed(&agent, "Choose a region.", finish, config)
        .await
        .unwrap();
    assert_eq!(replay.final_output(), completed.final_output());
    assert_eq!(
        store
            .load_checkpoint(&handle.checkpoint_key)
            .unwrap()
            .unwrap(),
        terminal
    );
    assert_eq!(
        models_called.load(Ordering::SeqCst),
        2 - usize::from(replaying) - usize::from(replaying_model)
    );
    assert_eq!(tools_called.load(Ordering::SeqCst), usize::from(!replaying));
}
