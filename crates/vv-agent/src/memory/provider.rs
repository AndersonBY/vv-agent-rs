use std::future::Future;
use std::pin::Pin;

use crate::events::RunEvent;
use crate::types::Metadata;

pub type MemoryFuture<T> = Pin<Box<dyn Future<Output = Result<T, MemoryError>> + Send>>;

pub trait MemoryProvider: Send + Sync {
    fn search(&self, request: MemorySearchRequest) -> MemoryFuture<Vec<MemorySearchResult>>;
    fn save(&self, request: MemorySaveRequest) -> MemoryFuture<MemorySaveResult>;

    fn before_compact(&self, _event: &RunEvent) -> MemoryFuture<MemoryProviderResult> {
        Box::pin(async { Ok(MemoryProviderResult::default()) })
    }

    fn after_compact(&self, _event: &RunEvent) -> MemoryFuture<()> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemorySearchRequest {
    pub query: String,
    pub metadata: Metadata,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemorySearchResult {
    pub content: String,
    pub score: Option<f64>,
    pub metadata: Metadata,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemorySaveRequest {
    pub content: String,
    pub metadata: Metadata,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemorySaveResult {
    pub id: Option<String>,
    pub metadata: Metadata,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemoryProviderResult {
    pub metadata: Metadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryError {
    message: String,
}

impl MemoryError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for MemoryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for MemoryError {}

pub(crate) fn block_on_memory_future<T: Send>(future: MemoryFuture<T>) -> Result<T, MemoryError> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
            tokio::task::block_in_place(|| handle.block_on(future))
        } else {
            handle.block_on(future)
        }
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| MemoryError::new(error.to_string()))?
            .block_on(future)
    }
}
