#![cfg(feature = "apalis")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use apalis::prelude::{
    Attempt, Backend, Extensions, MemoryStorage, RandomId, Status, Task, TaskBuilder, TaskId,
    TaskResult, TaskSink, TaskSinkError, WaitForCompletion, WorkerContext,
};
use futures_util::stream::{self, BoxStream};
use futures_util::StreamExt;
use serde_json::json;
use vv_agent::runtime::backends::distributed::{
    apalis::{
        run_apalis_cycle_job, run_apalis_worker_task, ApalisCycleDispatcher, ApalisCycleJob,
        ApalisResultCycleDispatcher,
    },
    CapabilityRef, CycleDispatcher, DistributedCapabilities, DistributedCapabilityRegistry,
    DistributedCheckpointConfig, DistributedCheckpointProgress, DistributedCycleWorker,
    DistributedRunEnvelope, DistributedV2CycleExecutor, DistributedV2CycleOutcome,
    ResolvedDistributedCapabilities, DEFAULT_LEASE_DURATION_MS,
};
use vv_agent::runtime::backends::{CycleDispatchResult, RuntimeRecipe};
use vv_agent::runtime::checkpoint_codec_v2::checkpoint_v2_from_value;
use vv_agent::{
    AgentResult, AgentTask, AmbiguousModelPolicy, AmbiguousToolPolicy, CheckpointStatus,
    CheckpointStoreV2, ClaimMode, InMemoryCheckpointStoreV2, Message, ResumePolicy,
    RunBudgetLimits,
};

const V2_FIXTURE: &str = include_str!("fixtures/parity/distributed_run_envelope_v2.json");
const CODEC_FIXTURE: &str = include_str!("fixtures/parity/checkpoint_codec_v2.json");

#[derive(Clone)]
struct CompletionBackend {
    configured_result: CycleDispatchResult,
    results: Arc<Mutex<std::collections::BTreeMap<String, CycleDispatchResult>>>,
}

impl CompletionBackend {
    fn new(configured_result: CycleDispatchResult) -> Self {
        Self {
            configured_result,
            results: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
        }
    }

    fn record(
        &self,
        task: &Task<ApalisCycleJob, Extensions, RandomId>,
    ) -> Result<(), std::io::Error> {
        let task_id = task
            .parts
            .task_id
            .as_ref()
            .ok_or_else(|| std::io::Error::other("task id missing"))?
            .to_string();
        self.results
            .lock()
            .map_err(|_| std::io::Error::other("result lock poisoned"))?
            .insert(task_id, self.configured_result.clone());
        Ok(())
    }
}

impl Backend for CompletionBackend {
    type Args = ApalisCycleJob;
    type IdType = RandomId;
    type Context = Extensions;
    type Error = std::io::Error;
    type Stream =
        stream::Empty<Result<Option<Task<ApalisCycleJob, Extensions, RandomId>>, Self::Error>>;
    type Beat = stream::Empty<Result<(), Self::Error>>;
    type Layer = ();

    fn heartbeat(&self, _worker: &WorkerContext) -> Self::Beat {
        stream::empty()
    }

    fn middleware(&self) -> Self::Layer {}

    fn poll(self, _worker: &WorkerContext) -> Self::Stream {
        stream::empty()
    }
}

impl TaskSink<ApalisCycleJob> for CompletionBackend {
    async fn push(&mut self, task: ApalisCycleJob) -> Result<(), TaskSinkError<Self::Error>> {
        self.push_task(Task::new(task)).await
    }

    async fn push_bulk(
        &mut self,
        tasks: Vec<ApalisCycleJob>,
    ) -> Result<(), TaskSinkError<Self::Error>> {
        for task in tasks {
            self.push(task).await?;
        }
        Ok(())
    }

    async fn push_stream(
        &mut self,
        mut tasks: impl futures_util::Stream<Item = ApalisCycleJob> + Unpin + Send,
    ) -> Result<(), TaskSinkError<Self::Error>> {
        while let Some(task) = tasks.next().await {
            self.push(task).await?;
        }
        Ok(())
    }

    async fn push_task(
        &mut self,
        task: Task<ApalisCycleJob, Self::Context, Self::IdType>,
    ) -> Result<(), TaskSinkError<Self::Error>> {
        self.record(&task).map_err(TaskSinkError::PushError)
    }

    async fn push_all(
        &mut self,
        mut tasks: impl futures_util::Stream<Item = Task<ApalisCycleJob, Self::Context, Self::IdType>>
            + Unpin
            + Send,
    ) -> Result<(), TaskSinkError<Self::Error>> {
        while let Some(task) = tasks.next().await {
            self.push_task(task).await?;
        }
        Ok(())
    }
}

impl WaitForCompletion<CycleDispatchResult> for CompletionBackend {
    type ResultStream =
        BoxStream<'static, Result<TaskResult<CycleDispatchResult, RandomId>, Self::Error>>;

    fn wait_for(
        &self,
        task_ids: impl IntoIterator<Item = TaskId<Self::IdType>>,
    ) -> Self::ResultStream {
        let task_id = task_ids.into_iter().next().expect("one task id");
        let results = self.results.clone();
        stream::unfold((task_id, results), |(task_id, results)| async move {
            loop {
                let result = results
                    .lock()
                    .map_err(|_| std::io::Error::other("result lock poisoned"))
                    .map(|mut results| results.remove(&task_id.to_string()));
                match result {
                    Ok(Some(result)) => {
                        return Some((
                            Ok(TaskResult::new(task_id.clone(), Status::Done, Ok(result))),
                            (task_id, results),
                        ))
                    }
                    Ok(None) => tokio::time::sleep(Duration::from_millis(1)).await,
                    Err(error) => return Some((Err(error), (task_id, results))),
                }
            }
        })
        .take(1)
        .boxed()
    }

    async fn check_status(
        &self,
        task_ids: impl IntoIterator<Item = TaskId<Self::IdType>> + Send,
    ) -> Result<Vec<TaskResult<CycleDispatchResult, Self::IdType>>, Self::Error> {
        let results = self
            .results
            .lock()
            .map_err(|_| std::io::Error::other("result lock poisoned"))?;
        Ok(task_ids
            .into_iter()
            .filter_map(|task_id| {
                results
                    .get(&task_id.to_string())
                    .cloned()
                    .map(|result| TaskResult::new(task_id, Status::Done, Ok(result)))
            })
            .collect())
    }
}

#[test]
fn apalis_result_dispatcher_returns_worker_candidate_from_completion_backend() {
    let payload: serde_json::Value = serde_json::from_str(V2_FIXTURE).unwrap();
    let envelope = DistributedRunEnvelope::from_dict(&payload["canonical_envelope"]).unwrap();
    let candidate = CycleDispatchResult::terminal_candidate(
        AgentResult::completed(Vec::new(), Vec::new(), "done"),
        7,
    );
    let dispatcher = ApalisResultCycleDispatcher::new(CompletionBackend::new(candidate.clone()));

    let result = dispatcher.dispatch_envelope(&envelope).unwrap();

    assert_eq!(result, candidate);
    assert!(result.terminal_candidate);
    assert!(!result.terminal_replay);
}

struct BlockingCheckpointExecutor {
    timer_ran: Arc<AtomicBool>,
}

impl DistributedV2CycleExecutor for BlockingCheckpointExecutor {
    fn execute(
        &self,
        envelope: &DistributedRunEnvelope,
        _capabilities: &ResolvedDistributedCapabilities,
        progress: &mut DistributedCheckpointProgress,
    ) -> Result<DistributedV2CycleOutcome, String> {
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            self.timer_ran.load(Ordering::SeqCst),
            "blocking v2 executor starved the current-thread Tokio runtime"
        );
        let mut checkpoint = progress.checkpoint().clone();
        checkpoint.cycle_index = u64::from(envelope.cycle_index);
        Ok(DistributedV2CycleOutcome::Continue(checkpoint))
    }
}

#[tokio::test]
async fn apalis_cycle_job_round_trips_through_apalis_task() {
    let job = ApalisCycleJob::new(
        AgentTask::new("apalis-cycle", "model", "system", "prompt"),
        RuntimeRecipe::new("settings.json", "deepseek", "deepseek-v4-pro", "."),
        "vv_agent.distributed.run_single_cycle",
        7,
    );
    let mut wire = serde_json::to_value(&job).expect("serialize Apalis job");
    wire["task"] = json!({
        "task_id": "apalis-cycle",
        "model": "model",
        "system_prompt": "system",
        "user_prompt": "prompt"
    });
    let decoded: ApalisCycleJob =
        serde_json::from_value(wire).expect("deserialize sparse Apalis job");

    let task: Task<ApalisCycleJob, Extensions, RandomId> = Task::new(decoded);
    let restored = ApalisCycleJob::from_apalis_task(task);

    assert_eq!(restored, job);
}

#[tokio::test]
async fn apalis_cycle_job_handler_returns_dispatch_result() {
    let job = ApalisCycleJob::new(
        AgentTask::new("apalis-handler", "model", "system", "prompt"),
        RuntimeRecipe::new("settings.json", "deepseek", "deepseek-v4-pro", "."),
        "vv_agent.distributed.run_single_cycle",
        2,
    );

    let result = run_apalis_cycle_job(job, |job| {
        assert_eq!(job.envelope.cycle_index, 2);
        Ok(CycleDispatchResult::finished(
            vv_agent::AgentResult::completed(vec![Message::assistant("done")], Vec::new(), "ok"),
        ))
    })
    .await
    .expect("apalis cycle handler");

    assert!(result.finished);
    assert_eq!(
        result.result.and_then(|result| result.final_answer),
        Some("ok".to_string())
    );
}

#[test]
fn apalis_task_round_trip_preserves_distributed_budget_limits() {
    let limits = RunBudgetLimits::builder()
        .max_total_tokens(4_096)
        .max_tool_calls(7)
        .build()
        .expect("valid run budget");
    let envelope = DistributedRunEnvelope::for_cycle(
        AgentTask::new("apalis-budget", "model", "system", "prompt"),
        RuntimeRecipe::new("settings.json", "deepseek", "deepseek-v4-pro", "."),
        3,
        "vv_agent.distributed.run_single_cycle",
        None,
        None,
        DEFAULT_LEASE_DURATION_MS,
        Some(limits.clone()),
    )
    .expect("valid distributed envelope");
    let task: Task<ApalisCycleJob, Extensions, RandomId> =
        Task::new(ApalisCycleJob::from_envelope(envelope));

    let restored = ApalisCycleJob::from_apalis_task(task);

    assert_eq!(restored.envelope.budget_limits, Some(limits));
}

#[test]
fn apalis_task_conversion_preserves_claim_mode_and_worker_consumes_attempt() {
    let payload: serde_json::Value = serde_json::from_str(V2_FIXTURE).unwrap();
    let envelope = DistributedRunEnvelope::from_dict(&payload["canonical_envelope"]).unwrap();
    assert_eq!(envelope.claim_mode, Some(ClaimMode::Recovery));
    let mut initial = envelope;
    initial.claim_mode = Some(ClaimMode::Continue);
    let task: Task<ApalisCycleJob, Extensions, RandomId> =
        TaskBuilder::new(ApalisCycleJob::from_envelope(initial))
            .with_attempt(Attempt::new_with_value(2))
            .build();

    let restored = ApalisCycleJob::from_apalis_task(task);

    assert_eq!(restored.envelope.claim_mode, Some(ClaimMode::Continue));
}

#[tokio::test(flavor = "current_thread")]
async fn apalis_cycle_handler_moves_blocking_execution_off_async_runtime() {
    let timer_ran = Arc::new(AtomicBool::new(false));
    let timer_ran_for_timer = timer_ran.clone();
    let timer_ran_for_handler = timer_ran.clone();
    let job = ApalisCycleJob::new(
        AgentTask::new("apalis-spawn-blocking", "model", "system", "prompt"),
        RuntimeRecipe::new("settings.json", "deepseek", "deepseek-v4-pro", "."),
        "vv_agent.distributed.run_single_cycle",
        1,
    );

    let timer = async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        timer_ran_for_timer.store(true, Ordering::SeqCst);
    };
    let execution = run_apalis_cycle_job(job, move |_| {
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            timer_ran_for_handler.load(Ordering::SeqCst),
            "blocking cycle handler starved the current-thread Tokio runtime"
        );
        Ok(CycleDispatchResult::unfinished())
    });

    let (result, ()) = tokio::join!(execution, timer);

    assert!(!result.unwrap().finished);
    assert!(timer_ran.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "current_thread")]
async fn apalis_worker_task_recovers_retry_without_blocking_async_runtime() {
    let codec: serde_json::Value = serde_json::from_str(CODEC_FIXTURE).unwrap();
    let mut payload = codec["valid_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "minimal_running")
        .unwrap()["payload"]
        .clone();
    payload["checkpoint_key"] = json!("apalis-v2-retry");
    payload["task_id"] = json!("apalis-v2-retry-task");
    payload["root_run_id"] = json!("apalis-v2-retry-run");
    payload["trace_id"] = json!("apalis-v2-retry-trace");
    let checkpoint = checkpoint_v2_from_value(&payload, 262_144).unwrap();
    let store = Arc::new(InMemoryCheckpointStoreV2::new());
    store.create_checkpoint_v2(checkpoint.clone()).unwrap();

    let checkpoint_ref = CapabilityRef::new("checkpoint.apalis-retry", "2").unwrap();
    let registry = DistributedCapabilityRegistry::new();
    registry.register_checkpoint_store(checkpoint_ref.clone(), store.clone());
    let mut recipe = RuntimeRecipe::new("settings.json", "test", "test-model", ".");
    recipe.capabilities = DistributedCapabilities {
        checkpoint_store_ref: Some(checkpoint_ref),
        ..DistributedCapabilities::default()
    };
    let mut retry_task = AgentTask::new(
        "apalis-v2-retry-task",
        "test-model",
        "You are a careful assistant.",
        "Summarize the status.",
    );
    retry_task.max_cycles = 10;
    retry_task.use_workspace = false;
    retry_task.exclude_tools = vec!["task_finish".to_string(), "ask_user".to_string()];
    retry_task.memory_compact_threshold = checkpoint.run_definition["runtime_controls"]
        ["memory_compact_threshold"]
        .as_u64()
        .expect("frozen memory threshold");
    let envelope = DistributedRunEnvelope::for_checkpoint_cycle(
        retry_task,
        recipe,
        1,
        "vv_agent.distributed.run_single_cycle",
        Some("apalis-v2-retry-run".to_string()),
        None,
        1_000,
        None,
        "apalis-v2-retry-run",
        "apalis-v2-retry-trace",
        checkpoint.run_definition_digest.clone(),
        ClaimMode::Continue,
        checkpoint.resume_attempt,
        DistributedCheckpointConfig {
            key: "apalis-v2-retry".to_string(),
            resume_policy: ResumePolicy::RequireExisting,
            ambiguous_model_policy: AmbiguousModelPolicy::RequireReconciliation,
            ambiguous_tool_policy: AmbiguousToolPolicy::RequireReconciliation,
            required_extension_namespaces: Vec::new(),
            max_extension_state_bytes: 262_144,
            credential_slots: Vec::new(),
        },
    )
    .unwrap();
    let timer_ran = Arc::new(AtomicBool::new(false));
    let worker = Arc::new(
        DistributedCycleWorker::new(registry).with_checkpoint_executor(Arc::new(
            BlockingCheckpointExecutor {
                timer_ran: timer_ran.clone(),
            },
        )),
    );
    let task: Task<ApalisCycleJob, Extensions, RandomId> =
        TaskBuilder::new(ApalisCycleJob::from_envelope(envelope))
            .with_attempt(Attempt::new_with_value(2))
            .build();
    let timer_ran_for_timer = timer_ran.clone();
    let timer = async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        timer_ran_for_timer.store(true, Ordering::SeqCst);
    };

    let (result, ()) = tokio::join!(run_apalis_worker_task(task, worker), timer);

    assert!(!result.unwrap().finished);
    assert!(timer_ran.load(Ordering::SeqCst));
    let persisted = store
        .load_checkpoint_v2("apalis-v2-retry")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.resume_attempt, 2);
    assert_eq!(persisted.cycle_index, 1);
}

#[test]
fn apalis_polling_dispatcher_replays_retained_terminal_without_owning_ack() {
    let codec: serde_json::Value = serde_json::from_str(CODEC_FIXTURE).unwrap();
    let mut payload = codec["valid_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "minimal_running")
        .unwrap()["payload"]
        .clone();
    payload["checkpoint_key"] = json!("apalis-v2-terminal");
    payload["task_id"] = json!("apalis-v2-task");
    payload["root_run_id"] = json!("apalis-v2-run");
    payload["trace_id"] = json!("apalis-v2-trace");
    let mut checkpoint = checkpoint_v2_from_value(&payload, 262_144).unwrap();
    checkpoint.cycle_index = 1;
    checkpoint.status = CheckpointStatus::Completed;
    checkpoint.terminal_result =
        Some(AgentResult::completed(Vec::new(), Vec::new(), "done").to_dict());
    checkpoint.revision = 4;
    let store = Arc::new(InMemoryCheckpointStoreV2::new());
    store.create_checkpoint_v2(checkpoint.clone()).unwrap();

    let checkpoint_ref = CapabilityRef::new("checkpoint.apalis", "2").unwrap();
    let mut recipe = RuntimeRecipe::new("settings.json", "test", "test-model", ".");
    recipe.capabilities = DistributedCapabilities {
        checkpoint_store_ref: Some(checkpoint_ref),
        ..DistributedCapabilities::default()
    };
    let envelope = DistributedRunEnvelope::for_checkpoint_cycle(
        AgentTask::new("apalis-v2-task", "test-model", "system", "prompt"),
        recipe,
        1,
        "vv_agent.distributed.run_single_cycle",
        Some("apalis-v2-run".to_string()),
        None,
        1_000,
        None,
        "apalis-v2-run",
        "apalis-v2-trace",
        checkpoint.run_definition_digest.clone(),
        ClaimMode::Continue,
        1,
        DistributedCheckpointConfig {
            key: "apalis-v2-terminal".to_string(),
            resume_policy: ResumePolicy::RequireExisting,
            ambiguous_model_policy: AmbiguousModelPolicy::RequireReconciliation,
            ambiguous_tool_policy: AmbiguousToolPolicy::RequireReconciliation,
            required_extension_namespaces: Vec::new(),
            max_extension_state_bytes: 262_144,
            credential_slots: Vec::new(),
        },
    )
    .unwrap();
    let backend: MemoryStorage<ApalisCycleJob> = MemoryStorage::new();
    let dispatcher = ApalisCycleDispatcher::new_v2(backend, store.clone())
        .with_poll_interval(Duration::from_millis(2));
    let result = dispatcher.dispatch_envelope(&envelope).unwrap();

    assert!(result.finished);
    assert!(result.terminal_replay);
    assert_eq!(result.checkpoint_revision, Some(4));
    assert!(
        !store
            .load_checkpoint_v2("apalis-v2-terminal")
            .unwrap()
            .unwrap()
            .terminal_acknowledged
    );
}

#[test]
fn apalis_polling_dispatcher_rejects_live_v2_before_enqueue() {
    let codec: serde_json::Value = serde_json::from_str(CODEC_FIXTURE).unwrap();
    let mut payload = codec["valid_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "minimal_running")
        .unwrap()["payload"]
        .clone();
    payload["checkpoint_key"] = json!("apalis-v2-live");
    payload["task_id"] = json!("apalis-v2-live-task");
    payload["root_run_id"] = json!("apalis-v2-live-run");
    payload["trace_id"] = json!("apalis-v2-live-trace");
    let checkpoint = checkpoint_v2_from_value(&payload, 262_144).unwrap();
    let store = Arc::new(InMemoryCheckpointStoreV2::new());
    store.create_checkpoint_v2(checkpoint.clone()).unwrap();

    let checkpoint_ref = CapabilityRef::new("checkpoint.apalis-live", "2").unwrap();
    let mut recipe = RuntimeRecipe::new("settings.json", "test", "test-model", ".");
    recipe.capabilities = DistributedCapabilities {
        checkpoint_store_ref: Some(checkpoint_ref),
        ..DistributedCapabilities::default()
    };
    let envelope = DistributedRunEnvelope::for_checkpoint_cycle(
        AgentTask::new(
            "apalis-v2-live-task",
            "test-model",
            "You are a careful assistant.",
            "Summarize the status.",
        ),
        recipe,
        1,
        "vv_agent.distributed.run_single_cycle",
        Some("apalis-v2-live-run".to_string()),
        None,
        1_000,
        None,
        "apalis-v2-live-run",
        "apalis-v2-live-trace",
        checkpoint.run_definition_digest.clone(),
        ClaimMode::Continue,
        checkpoint.resume_attempt,
        DistributedCheckpointConfig {
            key: "apalis-v2-live".to_string(),
            resume_policy: ResumePolicy::RequireExisting,
            ambiguous_model_policy: AmbiguousModelPolicy::RequireReconciliation,
            ambiguous_tool_policy: AmbiguousToolPolicy::RequireReconciliation,
            required_extension_namespaces: Vec::new(),
            max_extension_state_bytes: 262_144,
            credential_slots: Vec::new(),
        },
    )
    .unwrap();
    let backend: MemoryStorage<ApalisCycleJob> = MemoryStorage::new();
    let dispatcher = ApalisCycleDispatcher::new_v2(backend, store.clone());

    let error = dispatcher.dispatch_envelope(&envelope).unwrap_err();

    assert_eq!(
        error,
        "Apalis checkpoint polling cannot transport checkpoint v2 terminal candidates; use ApalisResultCycleDispatcher with a durable WaitForCompletion backend"
    );
    let persisted = store.load_checkpoint_v2("apalis-v2-live").unwrap().unwrap();
    assert_eq!(persisted.revision, 0);
    assert!(persisted.claim_token.is_none());
}
