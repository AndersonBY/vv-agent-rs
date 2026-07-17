use std::future::Future;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use apalis::prelude::{
    BoxDynError, Status, Task, TaskBuilder, TaskId, TaskSink, WaitForCompletion,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use super::contract::{now_unix_ms, DistributedRunEnvelope, DEFAULT_LEASE_DURATION_MS};
use super::{
    CycleDispatchResult, CycleDispatcher, DistributedCycleWorker, DistributedDeliveryMetadata,
};
use crate::runtime::backends::RuntimeRecipe;
use crate::runtime::state::StateStore;
use crate::runtime::state_v2::CheckpointStoreV2;
use crate::runtime::CancellationToken;
use crate::types::AgentTask;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ApalisCycleJob {
    pub envelope: DistributedRunEnvelope,
}

impl ApalisCycleJob {
    pub fn new(
        task: AgentTask,
        recipe: RuntimeRecipe,
        cycle_name: impl Into<String>,
        cycle_index: u32,
    ) -> Self {
        Self::from_envelope(
            DistributedRunEnvelope::for_cycle(
                task,
                recipe,
                cycle_index,
                cycle_name,
                None,
                None,
                DEFAULT_LEASE_DURATION_MS,
                None,
            )
            .expect("ApalisCycleJob inputs must satisfy the distributed envelope contract"),
        )
    }

    pub fn from_envelope(envelope: DistributedRunEnvelope) -> Self {
        Self { envelope }
    }

    pub fn from_apalis_task<Ctx, IdType>(task: Task<Self, Ctx, IdType>) -> Self {
        task.args
    }

    pub fn into_envelope(self) -> DistributedRunEnvelope {
        self.envelope
    }
}

pub async fn run_apalis_worker_job(
    job: ApalisCycleJob,
    worker: Arc<DistributedCycleWorker>,
) -> Result<CycleDispatchResult, BoxDynError> {
    let result = tokio::task::spawn_blocking(move || worker.run_cycle(job.into_envelope()))
        .await
        .map_err(|error| BoxDynError::from(std::io::Error::other(error.to_string())))?;
    result.map_err(|error| BoxDynError::from(std::io::Error::other(error)))
}

pub async fn run_apalis_worker_task<Ctx, IdType>(
    task: Task<ApalisCycleJob, Ctx, IdType>,
    worker: Arc<DistributedCycleWorker>,
) -> Result<CycleDispatchResult, BoxDynError> {
    let attempt = u64::try_from(task.parts.attempt.current())
        .map_err(|error| BoxDynError::from(std::io::Error::other(error.to_string())))?;
    let delivery = DistributedDeliveryMetadata {
        redelivered: attempt > 1,
        attempt,
    };
    let job = task.args;
    let result = tokio::task::spawn_blocking(move || {
        worker.run_cycle_with_delivery(job.into_envelope(), delivery)
    })
    .await
    .map_err(|error| BoxDynError::from(std::io::Error::other(error.to_string())))?;
    result.map_err(|error| BoxDynError::from(std::io::Error::other(error)))
}

/// Compatibility bridge for applications that own a custom worker runtime.
/// New integrations should use [`run_apalis_worker_job`].
pub async fn run_apalis_cycle_job<F>(
    job: ApalisCycleJob,
    cycle_handler: F,
) -> Result<CycleDispatchResult, BoxDynError>
where
    F: FnOnce(ApalisCycleJob) -> Result<CycleDispatchResult, String> + Send + 'static,
{
    let result = tokio::task::spawn_blocking(move || cycle_handler(job))
        .await
        .map_err(|error| BoxDynError::from(std::io::Error::other(error.to_string())))?;
    result.map_err(|error| BoxDynError::from(std::io::Error::other(error)))
}

pub struct ApalisCycleDispatcher<B> {
    backend: Arc<Mutex<B>>,
    state_store: Option<Arc<dyn StateStore>>,
    checkpoint_store: Option<Arc<dyn CheckpointStoreV2>>,
    poll_interval: Duration,
}

pub struct ApalisResultCycleDispatcher<B> {
    backend: Arc<Mutex<B>>,
}

impl<B> Clone for ApalisResultCycleDispatcher<B> {
    fn clone(&self) -> Self {
        Self {
            backend: self.backend.clone(),
        }
    }
}

impl<B> std::fmt::Debug for ApalisResultCycleDispatcher<B> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApalisResultCycleDispatcher")
            .finish_non_exhaustive()
    }
}

impl<B> ApalisResultCycleDispatcher<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend: Arc::new(Mutex::new(backend)),
        }
    }
}

impl<B> Clone for ApalisCycleDispatcher<B> {
    fn clone(&self) -> Self {
        Self {
            backend: self.backend.clone(),
            state_store: self.state_store.clone(),
            checkpoint_store: self.checkpoint_store.clone(),
            poll_interval: self.poll_interval,
        }
    }
}

impl<B> std::fmt::Debug for ApalisCycleDispatcher<B> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApalisCycleDispatcher")
            .field("poll_interval", &self.poll_interval)
            .finish_non_exhaustive()
    }
}

impl<B> ApalisCycleDispatcher<B> {
    pub fn new(backend: B, state_store: Arc<dyn StateStore>) -> Self {
        Self {
            backend: Arc::new(Mutex::new(backend)),
            state_store: Some(state_store),
            checkpoint_store: None,
            poll_interval: Duration::from_millis(100),
        }
    }

    pub fn new_v2(backend: B, checkpoint_store: Arc<dyn CheckpointStoreV2>) -> Self {
        Self {
            backend: Arc::new(Mutex::new(backend)),
            state_store: None,
            checkpoint_store: Some(checkpoint_store),
            poll_interval: Duration::from_millis(100),
        }
    }

    pub fn with_poll_interval(mut self, poll_interval: Duration) -> Self {
        assert!(!poll_interval.is_zero(), "poll interval must be positive");
        self.poll_interval = poll_interval;
        self
    }
}

impl<B> ApalisCycleDispatcher<B>
where
    B: TaskSink<ApalisCycleJob> + Send,
    B::Error: std::fmt::Display,
{
    fn dispatch_envelope_and_wait(
        &self,
        envelope: &DistributedRunEnvelope,
        cancellation_token: Option<&CancellationToken>,
    ) -> Result<CycleDispatchResult, String> {
        check_cancellation(cancellation_token)?;
        envelope.validate()?;
        envelope.ensure_not_expired()?;
        if envelope.is_checkpoint_v2() {
            let store = self
                .checkpoint_store
                .as_ref()
                .ok_or_else(|| "Apalis v2 dispatch requires a CheckpointStoreV2".to_string())?;
            let key = &envelope
                .checkpoint_config
                .as_ref()
                .expect("validated v2 envelope has checkpoint_config")
                .key;
            let checkpoint = store
                .load_checkpoint_v2(key)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("No checkpoint found for key {key}"))?;
            if let Some(result) = checkpoint.terminal_result.as_ref() {
                return Ok(CycleDispatchResult::terminal_replay(
                    crate::types::AgentResult::from_dict(result)?,
                    checkpoint.revision,
                ));
            }
            if checkpoint.claim_token.is_none()
                && checkpoint.cycle_index >= u64::from(envelope.cycle_index)
            {
                return Ok(CycleDispatchResult::committed(
                    checkpoint.cycle_index,
                    checkpoint.revision,
                ));
            }
            return Err(
                "Apalis checkpoint polling cannot transport checkpoint v2 terminal candidates; use ApalisResultCycleDispatcher with a durable WaitForCompletion backend"
                    .to_string(),
            );
        }
        let job = ApalisCycleJob::from_envelope(envelope.clone());
        let task: Task<ApalisCycleJob, B::Context, B::IdType> = TaskBuilder::new(job)
            .with_idempotency_key(&envelope.idempotency_key)
            .build();
        {
            let mut backend = self
                .backend
                .lock()
                .map_err(|_| "Apalis backend lock poisoned".to_string())?;
            block_on_apalis(backend.push_task(task))?
                .map_err(|error| format!("failed to enqueue Apalis cycle: {error}"))?;
        }

        loop {
            check_cancellation(cancellation_token)?;
            if envelope.is_checkpoint_v2() {
                let store = self
                    .checkpoint_store
                    .as_ref()
                    .ok_or_else(|| "Apalis v2 dispatch requires a CheckpointStoreV2".to_string())?;
                let key = &envelope
                    .checkpoint_config
                    .as_ref()
                    .expect("validated v2 envelope has checkpoint_config")
                    .key;
                let checkpoint = store
                    .load_checkpoint_v2(key)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| format!("No checkpoint found for key {key}"))?;
                if let Some(result) = checkpoint.terminal_result.as_ref() {
                    if checkpoint.terminal_acknowledged {
                        return Ok(CycleDispatchResult::finished_at_revision(
                            crate::types::AgentResult::from_dict(result)?,
                            Some(checkpoint.revision),
                        ));
                    }
                } else if checkpoint.claim_token.is_none()
                    && (checkpoint.cycle_index >= u64::from(envelope.cycle_index)
                        || checkpoint.status == crate::CheckpointStatus::ReconciliationRequired)
                {
                    return Ok(CycleDispatchResult::unfinished());
                }
            } else {
                let store = self
                    .state_store
                    .as_ref()
                    .ok_or_else(|| "Apalis v1 dispatch requires a StateStore".to_string())?;
                let checkpoint = store
                    .load_checkpoint(&envelope.task.task_id)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| {
                        format!("No checkpoint found for task {}", envelope.task.task_id)
                    })?;
                if let Some(result) = checkpoint.terminal_result {
                    return Ok(CycleDispatchResult::finished_at_revision(
                        result,
                        Some(checkpoint.revision),
                    ));
                }
                if checkpoint.cycle_index >= envelope.cycle_index
                    && checkpoint.claim_token.is_none()
                {
                    return Ok(CycleDispatchResult::unfinished());
                }
            }
            envelope.ensure_not_expired()?;
            check_cancellation(cancellation_token)?;
            std::thread::sleep(self.poll_interval);
        }
    }
}

impl<B> ApalisResultCycleDispatcher<B>
where
    B: TaskSink<ApalisCycleJob> + WaitForCompletion<CycleDispatchResult> + Send,
    B::Error: std::fmt::Display,
    B::IdType: Clone + FromStr + Send + Sync + 'static,
    <B::IdType as FromStr>::Err: std::fmt::Display,
{
    fn dispatch_envelope_and_wait(
        &self,
        envelope: &DistributedRunEnvelope,
        cancellation_token: Option<&CancellationToken>,
    ) -> Result<CycleDispatchResult, String> {
        check_cancellation(cancellation_token)?;
        envelope.validate()?;
        envelope.ensure_not_expired()?;
        let task_id = TaskId::<B::IdType>::from_str(&envelope.job_id)
            .map_err(|error| format!("invalid Apalis task id: {error}"))?;
        let task: Task<ApalisCycleJob, B::Context, B::IdType> =
            TaskBuilder::new(ApalisCycleJob::from_envelope(envelope.clone()))
                .with_task_id(task_id.clone())
                .with_idempotency_key(&envelope.idempotency_key)
                .build();
        let result_stream = {
            let mut backend = self
                .backend
                .lock()
                .map_err(|_| "Apalis backend lock poisoned".to_string())?;
            let result_stream = backend.wait_for_single(task_id);
            block_on_apalis(backend.push_task(task))?
                .map_err(|error| format!("failed to enqueue Apalis cycle: {error}"))?;
            result_stream
        };
        let cancellation_token = cancellation_token.cloned();
        let deadline_unix_ms = envelope.deadline_unix_ms;
        block_on_apalis(async move {
            let mut result_stream = Box::pin(result_stream);
            loop {
                check_cancellation(cancellation_token.as_ref())?;
                if deadline_unix_ms
                    .is_some_and(|deadline| now_unix_ms().map_or(true, |now_ms| now_ms >= deadline))
                {
                    return Err(
                        "Apalis result wait exceeded the distributed dispatch deadline".to_string(),
                    );
                }
                match tokio::time::timeout(Duration::from_millis(100), result_stream.next()).await {
                    Ok(Some(Ok(task_result))) => match task_result.status {
                        Status::Done => return task_result.take(),
                        Status::Failed | Status::Killed => {
                            return Err(task_result.take().err().unwrap_or_else(|| {
                                "Apalis task failed without an error".to_string()
                            }))
                        }
                        Status::Pending | Status::Queued | Status::Running => continue,
                        _ => continue,
                    },
                    Ok(Some(Err(error))) => {
                        return Err(format!("Apalis result backend failed: {error}"))
                    }
                    Ok(None) => {
                        return Err("Apalis result stream closed before task completion".to_string())
                    }
                    Err(_) => continue,
                }
            }
        })?
    }
}

impl<B> CycleDispatcher for ApalisResultCycleDispatcher<B>
where
    B: TaskSink<ApalisCycleJob> + WaitForCompletion<CycleDispatchResult> + Send,
    B::Error: std::fmt::Display,
    B::IdType: Clone + FromStr + Send + Sync + 'static,
    <B::IdType as FromStr>::Err: std::fmt::Display,
{
    fn dispatch_cycle(
        &self,
        task: &AgentTask,
        recipe: &RuntimeRecipe,
        cycle_name: &str,
        cycle_index: u32,
    ) -> Result<CycleDispatchResult, String> {
        let now_ms = now_unix_ms()?;
        let envelope = DistributedRunEnvelope::for_cycle(
            task.clone(),
            recipe.clone(),
            cycle_index,
            cycle_name,
            None,
            now_ms.checked_add(10 * 60 * 1000),
            DEFAULT_LEASE_DURATION_MS,
            None,
        )?;
        self.dispatch_envelope(&envelope)
    }

    fn dispatch_envelope(
        &self,
        envelope: &DistributedRunEnvelope,
    ) -> Result<CycleDispatchResult, String> {
        self.dispatch_envelope_and_wait(envelope, None)
    }

    fn dispatch_envelope_with_cancellation(
        &self,
        envelope: &DistributedRunEnvelope,
        cancellation_token: Option<&CancellationToken>,
    ) -> Result<CycleDispatchResult, String> {
        self.dispatch_envelope_and_wait(envelope, cancellation_token)
    }
}

impl<B> CycleDispatcher for ApalisCycleDispatcher<B>
where
    B: TaskSink<ApalisCycleJob> + Send,
    B::Error: std::fmt::Display,
{
    fn dispatch_cycle(
        &self,
        task: &AgentTask,
        recipe: &RuntimeRecipe,
        cycle_name: &str,
        cycle_index: u32,
    ) -> Result<CycleDispatchResult, String> {
        let now_ms = now_unix_ms()?;
        let envelope = DistributedRunEnvelope::for_cycle(
            task.clone(),
            recipe.clone(),
            cycle_index,
            cycle_name,
            None,
            now_ms.checked_add(10 * 60 * 1000),
            DEFAULT_LEASE_DURATION_MS,
            None,
        )?;
        self.dispatch_envelope(&envelope)
    }

    fn dispatch_envelope(
        &self,
        envelope: &DistributedRunEnvelope,
    ) -> Result<CycleDispatchResult, String> {
        self.dispatch_envelope_and_wait(envelope, None)
    }

    fn dispatch_envelope_with_cancellation(
        &self,
        envelope: &DistributedRunEnvelope,
        cancellation_token: Option<&CancellationToken>,
    ) -> Result<CycleDispatchResult, String> {
        self.dispatch_envelope_and_wait(envelope, cancellation_token)
    }
}

fn check_cancellation(cancellation_token: Option<&CancellationToken>) -> Result<(), String> {
    cancellation_token
        .map(CancellationToken::check)
        .transpose()
        .map(|_| ())
        .map_err(|reason| {
            format!(
                "Apalis dispatch cancelled while waiting; queued or claimed work may still complete: {reason}"
            )
        })
}

fn block_on_apalis<T>(future: impl Future<Output = T>) -> Result<T, String> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
            return Ok(tokio::task::block_in_place(|| handle.block_on(future)));
        }
        return Err(
            "Apalis dispatch cannot synchronously wait inside a current-thread Tokio runtime"
                .to_string(),
        );
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())
        .map(|runtime| runtime.block_on(future))
}
