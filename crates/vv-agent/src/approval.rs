use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use uuid::Uuid;

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
            request_id: new_approval_request_id(),
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

pub(crate) fn new_approval_request_id() -> String {
    format!("approval_{}", Uuid::new_v4().simple())
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
    state: Mutex<ApprovalBrokerState>,
    changed: Condvar,
}

#[derive(Default)]
struct ApprovalBrokerState {
    pending: HashMap<String, PendingApproval>,
    session_allowed_tools: HashSet<String>,
    cancel_decision: Option<ApprovalDecision>,
}

struct PendingApproval {
    request: ApprovalRequest,
    decision: Option<ApprovalDecision>,
}

impl ApprovalBroker {
    pub fn register(&self, request: ApprovalRequest) -> Result<(), ApprovalError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| ApprovalError::new("approval broker lock poisoned"))?;
        let decision = state.cancel_decision.clone().or_else(|| {
            state
                .session_allowed_tools
                .contains(&request.tool_name)
                .then_some(ApprovalDecision::ApprovedForSession)
        });
        state.pending.insert(
            request.request_id.clone(),
            PendingApproval { request, decision },
        );
        self.inner.changed.notify_all();
        Ok(())
    }

    pub fn resolve(
        &self,
        request_id: impl AsRef<str>,
        decision: ApprovalDecision,
    ) -> Result<(), ApprovalError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| ApprovalError::new("approval broker lock poisoned"))?;
        let request_id = request_id.as_ref();
        let Some(tool_name) = state
            .pending
            .get(request_id)
            .filter(|entry| entry.decision.is_none())
            .map(|entry| entry.request.tool_name.clone())
        else {
            return Err(ApprovalError::new(format!(
                "unknown approval request: {request_id}"
            )));
        };
        let decision = state.cancel_decision.clone().unwrap_or(decision);
        if decision.action() == "allow_session" {
            state.session_allowed_tools.insert(tool_name.clone());
        }
        if let Some(entry) = state.pending.get_mut(request_id) {
            entry.decision = Some(decision);
        }
        self.inner.changed.notify_all();
        Ok(())
    }

    pub(crate) fn allows_tool_for_session(&self, tool_name: &str) -> Result<bool, ApprovalError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| ApprovalError::new("approval broker lock poisoned"))?;
        Ok(state.cancel_decision.is_none() && state.session_allowed_tools.contains(tool_name))
    }

    #[cfg(test)]
    pub(crate) fn allow_tool_for_session(&self, tool_name: &str) -> Result<(), ApprovalError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| ApprovalError::new("approval broker lock poisoned"))?;
        if state.cancel_decision.is_some() {
            return Ok(());
        }
        state.session_allowed_tools.insert(tool_name.to_string());
        for entry in state
            .pending
            .values_mut()
            .filter(|entry| entry.request.tool_name == tool_name)
        {
            entry.decision = Some(ApprovalDecision::ApprovedForSession);
        }
        self.inner.changed.notify_all();
        Ok(())
    }

    pub fn pending_request(&self, request_id: impl AsRef<str>) -> Option<ApprovalRequest> {
        self.inner.state.lock().ok().and_then(|state| {
            state
                .pending
                .get(request_id.as_ref())
                .filter(|entry| entry.decision.is_none())
                .map(|entry| entry.request.clone())
        })
    }

    pub(crate) fn discard(&self, request_id: &str) -> Result<bool, ApprovalError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| ApprovalError::new("approval broker lock poisoned"))?;
        let removed = state.pending.remove(request_id).is_some();
        if removed {
            self.inner.changed.notify_all();
        }
        Ok(removed)
    }

    pub fn cancel_pending(&self, reason: impl Into<String>) -> Result<usize, ApprovalError> {
        let decision = ApprovalDecision::deny(reason.into());
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| ApprovalError::new("approval broker lock poisoned"))?;
        state.cancel_decision = Some(decision.clone());
        let pending_count = state
            .pending
            .values()
            .filter(|entry| entry.decision.is_none())
            .count();
        for entry in state
            .pending
            .values_mut()
            .filter(|entry| entry.decision.is_none())
        {
            entry.decision = Some(decision.clone());
        }
        self.inner.changed.notify_all();
        Ok(pending_count)
    }

    pub(crate) fn reset_cancelled(&self) -> Result<(), ApprovalError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| ApprovalError::new("approval broker lock poisoned"))?;
        state.cancel_decision = None;
        Ok(())
    }

    pub(crate) fn wait_blocking(
        &self,
        request_id: &str,
        timeout: Option<Duration>,
    ) -> Result<ApprovalDecision, ApprovalError> {
        let started = Instant::now();
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| ApprovalError::new("approval broker lock poisoned"))?;
        loop {
            if let Some(decision) = state
                .pending
                .get(request_id)
                .and_then(|entry| entry.decision.clone())
            {
                state.pending.remove(request_id);
                return Ok(decision);
            }

            if let Some(timeout) = timeout {
                let elapsed = started.elapsed();
                if elapsed >= timeout {
                    state.pending.remove(request_id);
                    return Ok(ApprovalDecision::timeout("Approval request timed out."));
                }
                let remaining = timeout.saturating_sub(elapsed);
                let (next_state, wait_result) =
                    self.inner
                        .changed
                        .wait_timeout(state, remaining)
                        .map_err(|_| ApprovalError::new("approval broker lock poisoned"))?;
                state = next_state;
                if wait_result.timed_out()
                    && state
                        .pending
                        .get(request_id)
                        .is_none_or(|entry| entry.decision.is_none())
                {
                    state.pending.remove(request_id);
                    return Ok(ApprovalDecision::timeout("Approval request timed out."));
                }
            } else {
                state = self
                    .inner
                    .changed
                    .wait(state)
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use serde_json::json;

    use super::{ApprovalBroker, ApprovalRequest};
    use crate::tools::ApprovalDecision;
    use crate::types::ToolCall;

    fn request(id: &str, tool_name: &str) -> ApprovalRequest {
        ApprovalRequest::for_tool_call(
            "run",
            "trace",
            "agent",
            0,
            &ToolCall::new(
                id,
                tool_name,
                BTreeMap::from([("path".to_string(), json!("file.txt"))]),
            ),
        )
    }

    #[test]
    fn cancel_pending_wakes_waiters_and_applies_to_future_registrations() {
        let broker = ApprovalBroker::default();
        let first = request("first", "dangerous_tool");
        let first_id = first.request_id.clone();
        broker.register(first).expect("register first request");

        let waiter = broker.clone();
        let join = std::thread::spawn(move || waiter.wait_blocking(&first_id, None));
        assert_eq!(
            broker
                .cancel_pending("Run was cancelled.")
                .expect("cancel pending"),
            1
        );
        assert!(matches!(
            join.join().expect("join waiter").expect("decision"),
            ApprovalDecision::Denied(reason) if reason == "Run was cancelled."
        ));

        let second = request("second", "dangerous_tool");
        let second_id = second.request_id.clone();
        broker.register(second).expect("register second request");
        assert!(matches!(
            broker
                .wait_blocking(&second_id, Some(Duration::from_millis(10)))
                .expect("future cancellation decision"),
            ApprovalDecision::Denied(reason) if reason == "Run was cancelled."
        ));

        let late = request("late", "dangerous_tool");
        let late_id = late.request_id.clone();
        broker.register(late).expect("register late request");
        assert!(broker
            .resolve(&late_id, ApprovalDecision::allow_session())
            .is_err());
        assert!(matches!(
            broker
                .wait_blocking(&late_id, Some(Duration::from_millis(10)))
                .expect("late cancellation decision"),
            ApprovalDecision::Denied(reason) if reason == "Run was cancelled."
        ));
        assert!(!broker
            .allows_tool_for_session("dangerous_tool")
            .expect("cancelled session grant"));
    }

    #[test]
    fn allow_session_grants_only_the_same_tool_for_the_broker_lifetime() {
        let broker = ApprovalBroker::default();
        let first = request("first", "dangerous_tool");
        let first_id = first.request_id.clone();
        broker.register(first).expect("register first request");
        broker
            .resolve(&first_id, ApprovalDecision::allow_session())
            .expect("allow tool for session");
        assert_eq!(
            broker
                .wait_blocking(&first_id, Some(Duration::from_millis(10)))
                .expect("session decision"),
            ApprovalDecision::ApprovedForSession
        );

        assert!(broker
            .allows_tool_for_session("dangerous_tool")
            .expect("session grant"));
        assert!(!broker
            .allows_tool_for_session("other_tool")
            .expect("other tool grant"));

        let repeated = request("repeated", "dangerous_tool");
        let repeated_id = repeated.request_id.clone();
        broker
            .register(repeated)
            .expect("register repeated request");
        assert_eq!(
            broker
                .wait_blocking(&repeated_id, Some(Duration::from_millis(10)))
                .expect("repeated decision"),
            ApprovalDecision::ApprovedForSession
        );
    }

    #[test]
    fn allow_deny_and_timeout_do_not_grant_session_access() {
        let broker = ApprovalBroker::default();
        let decisions = [
            ApprovalDecision::allow(),
            ApprovalDecision::deny("not allowed"),
            ApprovalDecision::timeout("too late"),
        ];

        for (index, decision) in decisions.into_iter().enumerate() {
            let request = request(&format!("call_{index}"), "dangerous_tool");
            let request_id = request.request_id.clone();
            broker.register(request).expect("register request");
            broker
                .resolve(&request_id, decision.clone())
                .expect("resolve request");
            assert_eq!(
                broker
                    .wait_blocking(&request_id, Some(Duration::from_millis(10)))
                    .expect("decision"),
                decision
            );
            assert!(!broker
                .allows_tool_for_session("dangerous_tool")
                .expect("session grant"));
        }
    }

    #[test]
    fn first_resolution_wins_until_the_waiter_consumes_it() {
        let broker = ApprovalBroker::default();
        let request = request("first-wins", "dangerous_tool");
        let request_id = request.request_id.clone();
        broker.register(request).expect("register request");
        broker
            .resolve(&request_id, ApprovalDecision::allow())
            .expect("resolve request");

        assert!(broker
            .resolve(&request_id, ApprovalDecision::deny("too late"))
            .is_err());
        assert!(broker.pending_request(&request_id).is_none());
        assert_eq!(
            broker
                .wait_blocking(&request_id, Some(Duration::from_millis(10)))
                .expect("first decision"),
            ApprovalDecision::Approved
        );
    }

    #[test]
    fn allow_session_does_not_resolve_an_already_pending_same_tool_request() {
        let broker = ApprovalBroker::default();
        let first = request("session-first", "dangerous_tool");
        let first_id = first.request_id.clone();
        let second = request("session-second", "dangerous_tool");
        let second_id = second.request_id.clone();
        broker.register(first).expect("register first request");
        broker.register(second).expect("register second request");

        broker
            .resolve(&first_id, ApprovalDecision::allow_session())
            .expect("resolve first request");
        assert_eq!(
            broker
                .wait_blocking(&first_id, Some(Duration::from_millis(10)))
                .expect("first decision"),
            ApprovalDecision::ApprovedForSession
        );
        assert!(broker.pending_request(&second_id).is_some());
        broker
            .resolve(&second_id, ApprovalDecision::allow())
            .expect("resolve second request");
        assert_eq!(
            broker
                .wait_blocking(&second_id, Some(Duration::from_millis(10)))
                .expect("second decision"),
            ApprovalDecision::Approved
        );
    }

    #[test]
    fn cancellation_preserves_an_existing_resolution_and_closes_future_requests() {
        let broker = ApprovalBroker::default();
        let resolved = request("resolved", "dangerous_tool");
        let resolved_id = resolved.request_id.clone();
        broker
            .register(resolved)
            .expect("register resolved request");
        broker
            .resolve(&resolved_id, ApprovalDecision::allow())
            .expect("resolve request");

        assert_eq!(
            broker.cancel_pending("cancelled").expect("cancel broker"),
            0
        );
        assert_eq!(
            broker
                .wait_blocking(&resolved_id, Some(Duration::from_millis(10)))
                .expect("existing decision"),
            ApprovalDecision::Approved
        );

        let future = request("future", "dangerous_tool");
        let future_id = future.request_id.clone();
        broker.register(future).expect("register future request");
        assert!(broker.pending_request(&future_id).is_none());
        assert!(broker
            .resolve(&future_id, ApprovalDecision::allow())
            .is_err());
        assert_eq!(
            broker
                .wait_blocking(&future_id, Some(Duration::from_millis(10)))
                .expect("cancellation decision"),
            ApprovalDecision::Denied("cancelled".to_string())
        );
    }
}
