use std::future::Future;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use apalis::prelude::{BoxDynError, Task, TaskBuilder, TaskId, TaskSink};
use serde::{Deserialize, Serialize};

use super::contract::DistributedRunEnvelope;
use super::{
    CycleDispatchResult, CycleEnqueuer, DistributedCycleWorker, DistributedDeliveryMetadata,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ApalisCycleJob {
    pub envelope: DistributedRunEnvelope,
}

impl ApalisCycleJob {
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

pub struct ApalisCycleEnqueuer<B> {
    backend: Arc<Mutex<B>>,
}

impl<B> Clone for ApalisCycleEnqueuer<B> {
    fn clone(&self) -> Self {
        Self {
            backend: self.backend.clone(),
        }
    }
}

impl<B> std::fmt::Debug for ApalisCycleEnqueuer<B> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApalisCycleEnqueuer")
            .finish_non_exhaustive()
    }
}

impl<B> ApalisCycleEnqueuer<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend: Arc::new(Mutex::new(backend)),
        }
    }
}

impl<B> CycleEnqueuer for ApalisCycleEnqueuer<B>
where
    B: TaskSink<ApalisCycleJob> + Send,
    B::Error: std::fmt::Display,
    B::IdType: Clone + FromStr + Send + Sync + 'static,
    <B::IdType as FromStr>::Err: std::fmt::Display,
{
    fn enqueue_envelope(
        &self,
        envelope: &DistributedRunEnvelope,
        not_before_unix_ms: Option<u64>,
    ) -> Result<(), String> {
        envelope.validate()?;
        envelope.ensure_not_expired()?;
        let task_id = TaskId::<B::IdType>::from_str(&envelope.job_id)
            .map_err(|error| format!("invalid Apalis task id: {error}"))?;
        let builder: TaskBuilder<ApalisCycleJob, B::Context, B::IdType> =
            TaskBuilder::new(ApalisCycleJob::from_envelope(envelope.clone()))
                .with_task_id(task_id)
                .with_idempotency_key(&envelope.idempotency_key);
        let task = match not_before_unix_ms {
            Some(not_before_unix_ms) => builder
                .run_at_timestamp(not_before_unix_ms.div_ceil(1_000))
                .build(),
            None => builder.build(),
        };
        let mut backend = self
            .backend
            .lock()
            .map_err(|_| "Apalis backend lock poisoned".to_string())?;
        block_on_apalis(backend.push_task(task))?
            .map_err(|error| format!("failed to enqueue Apalis cycle: {error}"))
    }
}

fn block_on_apalis<T>(future: impl Future<Output = T>) -> Result<T, String> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
            return Ok(tokio::task::block_in_place(|| handle.block_on(future)));
        }
        return Err(
            "Apalis enqueue cannot synchronously wait inside a current-thread Tokio runtime"
                .to_string(),
        );
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())
        .map(|runtime| runtime.block_on(future))
}
