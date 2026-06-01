use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::tools::ApprovalDecision;
use crate::types::{Metadata, ToolCall};

pub type ApprovalFuture<T> = Pin<Box<dyn Future<Output = Result<T, ApprovalError>> + Send>>;

pub trait ApprovalProvider: Send + Sync {
    fn should_request(&self, request: &ApprovalRequest) -> bool;
    fn decide(&self, request: &ApprovalRequest) -> ApprovalFuture<Option<ApprovalDecision>>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalRequest {
    pub request_id: String,
    pub run_id: String,
    pub trace_id: String,
    pub agent_name: String,
    pub cycle_index: u32,
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments: Value,
    pub preview: String,
    pub metadata: Metadata,
}

impl ApprovalRequest {
    pub fn for_tool_call(
        run_id: impl Into<String>,
        trace_id: impl Into<String>,
        agent_name: impl Into<String>,
        cycle_index: u32,
        call: &ToolCall,
    ) -> Self {
        let run_id = run_id.into();
        let trace_id = trace_id.into();
        let agent_name = agent_name.into();
        let arguments = Value::Object(call.arguments.clone().into_iter().collect());
        Self {
            request_id: format!("approval:{run_id}:{}:{}", call.name, call.id),
            run_id,
            trace_id,
            agent_name,
            cycle_index,
            tool_call_id: call.id.clone(),
            tool_name: call.name.clone(),
            preview: format!("{} {}", call.name, arguments),
            arguments,
            metadata: Metadata::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalError {
    message: String,
}

impl ApprovalError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for ApprovalError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ApprovalError {}

#[derive(Clone, Default)]
pub struct ApprovalBroker {
    inner: Arc<ApprovalBrokerInner>,
}

#[derive(Default)]
struct ApprovalBrokerInner {
    pending: Mutex<HashMap<String, PendingApproval>>,
    changed: Condvar,
}

struct PendingApproval {
    request: ApprovalRequest,
    decision: Option<ApprovalDecision>,
}

impl ApprovalBroker {
    pub fn register(&self, request: ApprovalRequest) -> Result<(), ApprovalError> {
        let mut pending = self
            .inner
            .pending
            .lock()
            .map_err(|_| ApprovalError::new("approval broker lock poisoned"))?;
        pending.insert(
            request.request_id.clone(),
            PendingApproval {
                request,
                decision: None,
            },
        );
        self.inner.changed.notify_all();
        Ok(())
    }

    pub fn resolve(
        &self,
        request_id: impl AsRef<str>,
        decision: ApprovalDecision,
    ) -> Result<(), ApprovalError> {
        let mut pending = self
            .inner
            .pending
            .lock()
            .map_err(|_| ApprovalError::new("approval broker lock poisoned"))?;
        let request_id = request_id.as_ref();
        let Some(entry) = pending.get_mut(request_id) else {
            return Err(ApprovalError::new(format!(
                "unknown approval request: {request_id}"
            )));
        };
        entry.decision = Some(decision);
        self.inner.changed.notify_all();
        Ok(())
    }

    pub fn pending_request(&self, request_id: impl AsRef<str>) -> Option<ApprovalRequest> {
        self.inner.pending.lock().ok().and_then(|pending| {
            pending
                .get(request_id.as_ref())
                .map(|entry| entry.request.clone())
        })
    }

    pub(crate) fn wait_blocking(
        &self,
        request_id: &str,
        timeout: Option<Duration>,
    ) -> Result<ApprovalDecision, ApprovalError> {
        let started = Instant::now();
        let mut pending = self
            .inner
            .pending
            .lock()
            .map_err(|_| ApprovalError::new("approval broker lock poisoned"))?;
        loop {
            if let Some(decision) = pending
                .get(request_id)
                .and_then(|entry| entry.decision.clone())
            {
                pending.remove(request_id);
                return Ok(decision);
            }

            if let Some(timeout) = timeout {
                let elapsed = started.elapsed();
                if elapsed >= timeout {
                    pending.remove(request_id);
                    return Ok(ApprovalDecision::timeout("approval timed out"));
                }
                let remaining = timeout.saturating_sub(elapsed);
                let (next_pending, wait_result) = self
                    .inner
                    .changed
                    .wait_timeout(pending, remaining)
                    .map_err(|_| ApprovalError::new("approval broker lock poisoned"))?;
                pending = next_pending;
                if wait_result.timed_out() {
                    pending.remove(request_id);
                    return Ok(ApprovalDecision::timeout("approval timed out"));
                }
            } else {
                pending = self
                    .inner
                    .changed
                    .wait(pending)
                    .map_err(|_| ApprovalError::new("approval broker lock poisoned"))?;
            }
        }
    }
}

pub(crate) fn block_on_approval_future<T: Send + 'static>(
    future: ApprovalFuture<T>,
) -> Result<T, ApprovalError> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
            tokio::task::block_in_place(|| handle.block_on(future))
        } else {
            std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| ApprovalError::new(error.to_string()))?
                    .block_on(future)
            })
            .join()
            .map_err(|_| ApprovalError::new("approval future thread panicked"))?
        }
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| ApprovalError::new(error.to_string()))?
            .block_on(future)
    }
}
