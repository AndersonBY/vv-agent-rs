use crate::runtime::backends::{
    DistributedBackend, InlineBackend, RuntimeExecutionBackend, ThreadBackend,
};

#[derive(Debug, Clone, Default)]
#[allow(clippy::large_enum_variant)] // Preserve direct backend construction in the public API.
pub enum ExecutionMode {
    #[default]
    Inline,
    Threaded {
        max_workers: usize,
    },
    Distributed(DistributedBackend),
}

impl From<ExecutionMode> for RuntimeExecutionBackend {
    fn from(mode: ExecutionMode) -> Self {
        match mode {
            ExecutionMode::Inline => Self::Inline(InlineBackend),
            ExecutionMode::Threaded { max_workers } => {
                Self::Thread(ThreadBackend::new(max_workers))
            }
            ExecutionMode::Distributed(backend) => Self::Distributed(backend),
        }
    }
}
