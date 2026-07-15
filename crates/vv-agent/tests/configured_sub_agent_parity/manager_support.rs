use super::*;

#[derive(Default)]
pub(super) struct ListenerRetainingSession {
    listeners: Mutex<Vec<SubAgentSessionListener>>,
}

impl ListenerRetainingSession {
    pub(super) fn emit(&self, event: &str, payload: BTreeMap<String, Value>) {
        let listeners = self
            .listeners
            .lock()
            .expect("retained session listeners")
            .clone();
        for listener in listeners {
            listener(event, &payload);
        }
    }
}

impl SubAgentSession for ListenerRetainingSession {
    fn steer(&self, _prompt: &str) -> Result<(), String> {
        Ok(())
    }

    fn subscribe(&self, listener: SubAgentSessionListener) -> Option<SubAgentSessionUnsubscribe> {
        self.listeners
            .lock()
            .expect("retained session listeners")
            .push(listener);
        None
    }
}

pub(super) struct FailedContinuationSession;

impl SubAgentSession for FailedContinuationSession {
    fn steer(&self, _prompt: &str) -> Result<(), String> {
        Ok(())
    }

    fn continue_run(&self, _prompt: &str) -> Result<SubTaskOutcome, String> {
        Ok(failed_outcome("continued-task", "continued-session"))
    }
}

pub(super) struct FailingSubscribeSession {
    pub(super) listeners: Mutex<Vec<SubAgentSessionListener>>,
    pub(super) subscribe_calls: AtomicUsize,
}

impl FailingSubscribeSession {
    pub(super) fn emit(&self, event: &str, payload: BTreeMap<String, Value>) {
        for listener in self.listeners.lock().expect("listeners").clone() {
            listener(event, &payload);
        }
    }
}

impl SubAgentSession for FailingSubscribeSession {
    fn steer(&self, _prompt: &str) -> Result<(), String> {
        Ok(())
    }

    fn subscribe(&self, listener: SubAgentSessionListener) -> Option<SubAgentSessionUnsubscribe> {
        if self.subscribe_calls.fetch_add(1, Ordering::SeqCst) == 0 {
            panic!("subscribe failed");
        }
        self.listeners.lock().expect("listeners").push(listener);
        None
    }
}

pub(super) fn failed_outcome(task_id: &str, session_id: &str) -> SubTaskOutcome {
    SubTaskOutcome {
        task_id: task_id.to_string(),
        agent_name: "researcher".to_string(),
        status: AgentStatus::Failed,
        session_id: Some(session_id.to_string()),
        final_answer: None,
        wait_reason: None,
        error: Some("child failed".to_string()),
        error_code: None,
        completion_reason: None,
        completion_tool_name: None,
        partial_output: None,
        cycles: 1,
        todo_list: Vec::new(),
        resolved: BTreeMap::new(),
    }
}

pub(super) fn assert_failed_outcome_code(manager: &SubTaskManager, task_id: &str, expected: &str) {
    let snapshot = manager.get(task_id).expect("failed task snapshot");
    assert_eq!(
        snapshot
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.error_code.as_deref()),
        Some(expected)
    );
    let entries = manager.status_entries(&[task_id.to_string()], "basic", 10);
    assert_eq!(entries[0]["error_code"], expected);
}

pub(super) fn assert_same_managed_snapshot(
    actual: &vv_agent::runtime::ManagedSubTaskSnapshot,
    expected: &vv_agent::runtime::ManagedSubTaskSnapshot,
) {
    assert_eq!(actual.task_id, expected.task_id);
    assert_eq!(actual.session_id, expected.session_id);
    assert_eq!(actual.agent_name, expected.agent_name);
    assert_eq!(actual.task_title, expected.task_title);
    assert_eq!(actual.status, expected.status);
    assert_eq!(actual.running, expected.running);
    assert_eq!(actual.outcome, expected.outcome);
    assert_eq!(actual.resolved, expected.resolved);
    assert_eq!(actual.current_cycle_index, expected.current_cycle_index);
    assert_eq!(actual.recent_activity, expected.recent_activity);
    assert_eq!(actual.latest_cycle, expected.latest_cycle);
    assert_eq!(actual.latest_tool_call, expected.latest_tool_call);
    assert_eq!(actual.parent_run_id, expected.parent_run_id);
    assert_eq!(actual.parent_tool_call_id, expected.parent_tool_call_id);
    assert_eq!(actual.updated_at, expected.updated_at);
}
