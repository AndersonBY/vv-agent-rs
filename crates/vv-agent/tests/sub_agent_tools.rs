use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex, MutexGuard, OnceLock,
};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};
use vv_agent::{
    _register_sub_agent_session, _unregister_sub_agent_session, build_default_registry,
    register_sub_agent_session, sub_agent_session_registry, unregister_sub_agent_session,
    AgentStatus, Message, RuntimeExecutionBackend, SubAgentSession, SubAgentSessionListener,
    SubTaskManager, SubTaskOutcome, SubTaskSessionAttachment, ThreadBackend, ToolCall, ToolContext,
    ToolResultStatus,
};

struct SubAgentRegistryTestLock {
    _guard: MutexGuard<'static, ()>,
}

impl Drop for SubAgentRegistryTestLock {
    fn drop(&mut self) {
        sub_agent_session_registry().clear();
    }
}

fn isolated_sub_agent_registry() -> SubAgentRegistryTestLock {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let guard = LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    sub_agent_session_registry().clear();
    SubAgentRegistryTestLock { _guard: guard }
}

#[path = "sub_agent_tools/async_status.rs"]
mod async_status;
#[path = "sub_agent_tools/continuation.rs"]
mod continuation;
#[path = "sub_agent_tools/create.rs"]
mod create;

struct RecordingSubAgentSession {
    received: Arc<Mutex<Vec<String>>>,
}

impl SubAgentSession for RecordingSubAgentSession {
    fn steer(&self, prompt: &str) -> Result<(), String> {
        self.received
            .lock()
            .expect("received")
            .push(prompt.to_string());
        Ok(())
    }
}

struct ContinuingSubAgentSession {
    continued: Arc<Mutex<Vec<String>>>,
}

impl SubAgentSession for ContinuingSubAgentSession {
    fn steer(&self, prompt: &str) -> Result<(), String> {
        self.continue_run(prompt).map(|_| ())
    }

    fn continue_run(&self, prompt: &str) -> Result<SubTaskOutcome, String> {
        self.continued
            .lock()
            .expect("continued")
            .push(prompt.to_string());
        thread::sleep(Duration::from_millis(25));
        Ok(SubTaskOutcome {
            task_id: "sub-task-completed".to_string(),
            agent_name: "researcher".to_string(),
            status: AgentStatus::Completed,
            session_id: Some("sub-session-continued".to_string()),
            final_answer: Some("continued done".to_string()),
            wait_reason: None,
            error: None,
            error_code: None,
            completion_reason: None,
            completion_tool_name: None,
            partial_output: None,
            cycles: 2,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        })
    }
}

struct SanitizingSubAgentSession {
    messages: Arc<Mutex<Vec<Message>>>,
    snapshot: Arc<Mutex<Vec<Message>>>,
}

impl SubAgentSession for SanitizingSubAgentSession {
    fn steer(&self, prompt: &str) -> Result<(), String> {
        self.continue_run(prompt).map(|_| ())
    }

    fn sanitize_for_resume(&self) -> usize {
        let mut messages = self.messages.lock().expect("messages");
        let sanitized = vv_agent::memory::sanitize_for_resume(&messages);
        let removed = messages.len().saturating_sub(sanitized.len());
        *messages = sanitized;
        removed
    }

    fn continue_run(&self, _prompt: &str) -> Result<SubTaskOutcome, String> {
        let mut messages = self.messages.lock().expect("messages");
        *self.snapshot.lock().expect("snapshot") = messages.clone();
        *messages = vec![Message::assistant("continued done")];
        Ok(SubTaskOutcome {
            task_id: "sub-sanitize".to_string(),
            agent_name: "researcher".to_string(),
            status: AgentStatus::Completed,
            session_id: Some("sub-session-sanitize".to_string()),
            final_answer: Some("continued done".to_string()),
            wait_reason: None,
            error: None,
            error_code: None,
            completion_reason: None,
            completion_tool_name: None,
            partial_output: None,
            cycles: 2,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        })
    }
}

#[derive(Default)]
struct EventingSubAgentSession {
    listeners: Mutex<Vec<SubAgentSessionListener>>,
}

impl EventingSubAgentSession {
    fn emit(&self, event: &str, payload: BTreeMap<String, Value>) {
        let listeners = self.listeners.lock().expect("listeners").clone();
        for listener in listeners {
            listener(event, &payload);
        }
    }
}

impl SubAgentSession for EventingSubAgentSession {
    fn steer(&self, _prompt: &str) -> Result<(), String> {
        Ok(())
    }

    fn subscribe(
        &self,
        listener: SubAgentSessionListener,
    ) -> Option<vv_agent::SubAgentSessionUnsubscribe> {
        self.listeners.lock().expect("listeners").push(listener);
        Some(Box::new(|| {}))
    }
}
