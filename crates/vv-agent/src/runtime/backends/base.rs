use super::distributed::DistributedBackend;
use super::inline::InlineBackend;
use super::thread::ThreadBackend;
use crate::runtime::CancellationToken;
use crate::types::{AgentResult, AgentTask, CycleRecord, Message, Metadata};

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)] // Preserve direct backend construction in the public API.
pub enum RuntimeExecutionBackend {
    Inline(InlineBackend),
    Thread(ThreadBackend),
    Distributed(DistributedBackend),
}

impl Default for RuntimeExecutionBackend {
    fn default() -> Self {
        Self::Inline(InlineBackend)
    }
}

impl From<InlineBackend> for RuntimeExecutionBackend {
    fn from(backend: InlineBackend) -> Self {
        Self::Inline(backend)
    }
}

impl From<ThreadBackend> for RuntimeExecutionBackend {
    fn from(backend: ThreadBackend) -> Self {
        Self::Thread(backend)
    }
}

impl From<DistributedBackend> for RuntimeExecutionBackend {
    fn from(backend: DistributedBackend) -> Self {
        Self::Distributed(backend)
    }
}

impl RuntimeExecutionBackend {
    pub fn execute<F>(
        &self,
        task: &AgentTask,
        initial_messages: Vec<Message>,
        shared_state: Metadata,
        cycle_executor: F,
        cancellation_token: Option<&CancellationToken>,
        max_cycles: u32,
    ) -> AgentResult
    where
        F: FnMut(
            u32,
            &mut Vec<Message>,
            &mut Vec<CycleRecord>,
            &mut Metadata,
            Option<&CancellationToken>,
        ) -> Option<AgentResult>,
    {
        match self {
            Self::Inline(backend) => backend.execute(
                task,
                initial_messages,
                shared_state,
                cycle_executor,
                cancellation_token,
                max_cycles,
            ),
            Self::Thread(backend) => backend.execute(
                task,
                initial_messages,
                shared_state,
                cycle_executor,
                cancellation_token,
                max_cycles,
            ),
            Self::Distributed(backend) => backend.execute(
                task,
                initial_messages,
                shared_state,
                cycle_executor,
                cancellation_token,
                max_cycles,
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn execute_with_state<F>(
        &self,
        task: &AgentTask,
        initial_messages: Vec<Message>,
        initial_cycles: Vec<CycleRecord>,
        shared_state: Metadata,
        cycle_executor: F,
        cancellation_token: Option<&CancellationToken>,
        cycle_index_start: u32,
        cycle_count: u32,
    ) -> AgentResult
    where
        F: FnMut(
            u32,
            &mut Vec<Message>,
            &mut Vec<CycleRecord>,
            &mut Metadata,
            Option<&CancellationToken>,
        ) -> Option<AgentResult>,
    {
        match self {
            Self::Inline(backend) => backend.execute_with_state(
                task,
                initial_messages,
                initial_cycles,
                shared_state,
                cycle_executor,
                cancellation_token,
                cycle_index_start,
                cycle_count,
            ),
            Self::Thread(backend) => backend.execute_with_state(
                task,
                initial_messages,
                initial_cycles,
                shared_state,
                cycle_executor,
                cancellation_token,
                cycle_index_start,
                cycle_count,
            ),
            Self::Distributed(backend) => backend.execute_with_state(
                task,
                initial_messages,
                initial_cycles,
                shared_state,
                cycle_executor,
                cancellation_token,
                cycle_index_start,
                cycle_count,
            ),
        }
    }

    pub fn parallel_map<T, R, F>(&self, function: F, items: Vec<T>) -> Vec<R>
    where
        T: Send + 'static,
        R: Send + 'static,
        F: Fn(T) -> R + Send + Sync + 'static,
    {
        match self {
            Self::Inline(backend) => backend.parallel_map(function, items),
            Self::Thread(backend) => backend.parallel_map(function, items),
            Self::Distributed(backend) => backend.parallel_map(function, items),
        }
    }
}
