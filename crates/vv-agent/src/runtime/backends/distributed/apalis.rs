use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use apalis::prelude::{BoxDynError, Task, TaskBuilder, TaskSink};
use serde::{Deserialize, Serialize};

use crate::runtime::backends::RuntimeRecipe;
use crate::runtime::state::StateStore;
use crate::runtime::CancellationToken;
use crate::types::AgentTask;

use super::contract::{now_unix_ms, DistributedRunEnvelope, DEFAULT_LEASE_DURATION_MS};
use super::{CycleDispatchResult, CycleDispatcher, DistributedCycleWorker};

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
    worker
        .run_cycle(job.into_envelope())
        .map_err(|error| BoxDynError::from(std::io::Error::other(error)))
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
    cycle_handler(job).map_err(|error| BoxDynError::from(std::io::Error::other(error)))
}

pub struct ApalisCycleDispatcher<B> {
    backend: Arc<Mutex<B>>,
    state_store: Arc<dyn StateStore>,
    poll_interval: Duration,
}

impl<B> Clone for ApalisCycleDispatcher<B> {
    fn clone(&self) -> Self {
        Self {
            backend: self.backend.clone(),
            state_store: self.state_store.clone(),
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
            state_store,
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
            let checkpoint = self
                .state_store
                .load_checkpoint(&envelope.task.task_id)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("No checkpoint found for task {}", envelope.task.task_id))?;
            if let Some(result) = checkpoint.terminal_result {
                return Ok(CycleDispatchResult::finished_at_revision(
                    result,
                    Some(checkpoint.revision),
                ));
            }
            if checkpoint.cycle_index >= envelope.cycle_index && checkpoint.claim_token.is_none() {
                return Ok(CycleDispatchResult::unfinished());
            }
            envelope.ensure_not_expired()?;
            check_cancellation(cancellation_token)?;
            std::thread::sleep(self.poll_interval);
        }
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
