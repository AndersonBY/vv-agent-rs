use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;

use crate::types::SubTaskOutcome;

pub type SubAgentSessionListener =
    Arc<dyn Fn(&str, &BTreeMap<String, Value>) + Send + Sync + 'static>;
pub type SubAgentSessionUnsubscribe = Box<dyn FnOnce() + Send + 'static>;

pub trait SubAgentSession: Send + Sync {
    fn steer(&self, prompt: &str) -> Result<(), String>;

    fn continue_run(&self, _prompt: &str) -> Result<SubTaskOutcome, String> {
        Err("Sub-agent session continuation is not supported.".to_string())
    }

    fn subscribe(&self, _listener: SubAgentSessionListener) -> Option<SubAgentSessionUnsubscribe> {
        None
    }
}

#[derive(Default)]
pub struct SubAgentSessionRegistry {
    sessions: Mutex<BTreeMap<String, Arc<dyn SubAgentSession>>>,
}

impl SubAgentSessionRegistry {
    pub fn register(&self, session_id: impl Into<String>, session: Arc<dyn SubAgentSession>) {
        let session_id = session_id.into();
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return;
        }
        self.sessions
            .lock()
            .expect("sub-agent session registry poisoned")
            .insert(session_id.to_string(), session);
    }

    pub fn unregister(&self, session_id: &str) {
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return;
        }
        self.sessions
            .lock()
            .expect("sub-agent session registry poisoned")
            .remove(session_id);
    }

    pub fn get(&self, session_id: &str) -> Option<Arc<dyn SubAgentSession>> {
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return None;
        }
        self.sessions
            .lock()
            .expect("sub-agent session registry poisoned")
            .get(session_id)
            .cloned()
    }

    pub fn clear(&self) {
        self.sessions
            .lock()
            .expect("sub-agent session registry poisoned")
            .clear();
    }
}

static ACTIVE_SUB_AGENT_SESSIONS: OnceLock<SubAgentSessionRegistry> = OnceLock::new();

pub fn sub_agent_session_registry() -> &'static SubAgentSessionRegistry {
    ACTIVE_SUB_AGENT_SESSIONS.get_or_init(SubAgentSessionRegistry::default)
}

pub fn register_sub_agent_session(
    session_id: impl Into<String>,
    session: Arc<dyn SubAgentSession>,
) {
    sub_agent_session_registry().register(session_id, session);
}

pub fn unregister_sub_agent_session(session_id: &str) {
    sub_agent_session_registry().unregister(session_id);
}

pub fn get_sub_agent_session(session_id: &str) -> Option<Arc<dyn SubAgentSession>> {
    sub_agent_session_registry().get(session_id)
}

pub fn steer_sub_agent_session(session_id: &str, prompt: &str) -> bool {
    let session_id = session_id.trim();
    let prompt = prompt.trim();
    if session_id.is_empty() || prompt.is_empty() {
        return false;
    }
    let Some(session) = get_sub_agent_session(session_id) else {
        return false;
    };
    session.steer(prompt).is_ok()
}

pub fn continue_sub_agent_session(
    session_id: &str,
    prompt: &str,
) -> Result<SubTaskOutcome, String> {
    let session_id = session_id.trim();
    let prompt = prompt.trim();
    if session_id.is_empty() {
        return Err("Sub-agent session id cannot be empty.".to_string());
    }
    if prompt.is_empty() {
        return Err("Follow-up prompt cannot be empty.".to_string());
    }
    let Some(session) = get_sub_agent_session(session_id) else {
        return Err(format!("Sub-agent session {session_id} is not registered."));
    };
    session.continue_run(prompt)
}

pub fn subscribe_sub_agent_session(
    session_id: &str,
    listener: SubAgentSessionListener,
) -> Option<SubAgentSessionUnsubscribe> {
    get_sub_agent_session(session_id)?.subscribe(listener)
}
