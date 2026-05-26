use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::types::{AgentStatus, SubTaskOutcome};

static SUB_TASK_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Default)]
pub struct SubTaskManager {
    tasks: Arc<Mutex<BTreeMap<String, ManagedSubTask>>>,
}

impl std::fmt::Debug for SubTaskManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SubTaskManager")
            .finish_non_exhaustive()
    }
}

impl SubTaskManager {
    pub fn next_task_identity(parent_task_id: &str, agent_name: &str) -> (String, String) {
        let parent = parent_task_id.trim();
        let parent = if parent.is_empty() { "task" } else { parent };
        let suffix = SUB_TASK_COUNTER.fetch_add(1, Ordering::Relaxed);
        let task_id = format!("{parent}_sub_{agent_name}_{suffix:08x}");
        (task_id.clone(), task_id)
    }

    pub fn submit(
        &self,
        task_id: impl Into<String>,
        session_id: impl Into<String>,
        agent_name: impl Into<String>,
        task_title: impl Into<String>,
        runner: impl FnOnce() -> SubTaskOutcome + Send + 'static,
    ) {
        let task_id = task_id.into();
        let session_id = session_id.into();
        let agent_name = agent_name.into();
        let task_title = task_title.into();
        {
            let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
            tasks.insert(
                task_id.clone(),
                ManagedSubTask {
                    task_id: task_id.clone(),
                    session_id: session_id.clone(),
                    agent_name: agent_name.clone(),
                    task_title,
                    outcome: None,
                    handle: None,
                    updated_at: now_millis(),
                },
            );
        }

        let tasks = self.tasks.clone();
        let task_id_for_thread = task_id.clone();
        let handle = thread::spawn(move || {
            let outcome = runner();
            let mut tasks = tasks.lock().expect("sub-task manager poisoned");
            if let Some(record) = tasks.get_mut(&task_id_for_thread) {
                record.outcome = Some(outcome);
                record.updated_at = now_millis();
                record.handle = None;
            }
        });

        let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
        if let Some(record) = tasks.get_mut(&task_id) {
            record.handle = Some(handle);
            record.updated_at = now_millis();
        }
    }

    pub fn record_outcome(&self, task_id: &str, outcome: SubTaskOutcome) {
        let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
        let task_id = task_id.to_string();
        match tasks.get_mut(&task_id) {
            Some(record) => {
                record.session_id = outcome
                    .session_id
                    .clone()
                    .unwrap_or_else(|| record.session_id.clone());
                record.agent_name = outcome.agent_name.clone();
                record.outcome = Some(outcome);
                record.updated_at = now_millis();
            }
            None => {
                tasks.insert(
                    task_id.clone(),
                    ManagedSubTask {
                        session_id: outcome.session_id.clone().unwrap_or_default(),
                        agent_name: outcome.agent_name.clone(),
                        task_title: String::new(),
                        outcome: Some(outcome),
                        task_id,
                        handle: None,
                        updated_at: now_millis(),
                    },
                );
            }
        }
    }

    pub fn status_entries(
        &self,
        task_ids: &[String],
        detail_level: &str,
        _workspace_file_limit: usize,
    ) -> Vec<Value> {
        let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
        for record in tasks.values_mut() {
            if record.handle.as_ref().is_some_and(JoinHandle::is_finished) {
                if let Some(handle) = record.handle.take() {
                    let _ = handle.join();
                }
            }
        }
        task_ids
            .iter()
            .map(|task_id| {
                let Some(record) = tasks.get(task_id) else {
                    return json!({
                        "task_id": task_id,
                        "status": "missing",
                        "error": format!("Sub-task {task_id} not found."),
                    });
                };
                record.to_status_entry(detail_level)
            })
            .collect()
    }

    pub fn task_session_id(&self, task_id: &str) -> Option<String> {
        self.join_finished_tasks();
        self.tasks
            .lock()
            .expect("sub-task manager poisoned")
            .get(task_id)
            .map(|record| record.session_id.clone())
    }

    pub fn task_status_label(&self, task_id: &str) -> Option<String> {
        self.join_finished_tasks();
        self.tasks
            .lock()
            .expect("sub-task manager poisoned")
            .get(task_id)
            .map(|record| record.status_label().to_string())
    }

    pub fn is_running(&self, task_id: &str) -> bool {
        self.join_finished_tasks();
        self.tasks
            .lock()
            .expect("sub-task manager poisoned")
            .get(task_id)
            .is_some_and(ManagedSubTask::is_running)
    }

    fn join_finished_tasks(&self) {
        let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
        for record in tasks.values_mut() {
            if record.handle.as_ref().is_some_and(JoinHandle::is_finished) {
                if let Some(handle) = record.handle.take() {
                    let _ = handle.join();
                }
            }
        }
    }
}

struct ManagedSubTask {
    task_id: String,
    session_id: String,
    agent_name: String,
    task_title: String,
    outcome: Option<SubTaskOutcome>,
    handle: Option<JoinHandle<()>>,
    updated_at: u128,
}

impl ManagedSubTask {
    fn is_running(&self) -> bool {
        self.handle
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
    }

    fn to_status_entry(&self, detail_level: &str) -> Value {
        let status = self.status_label();
        let mut entry = json!({
            "task_id": self.task_id,
            "session_id": self.session_id,
            "agent_name": self.agent_name,
            "status": status,
            "task_description": self.task_title,
        });
        if let Some(outcome) = &self.outcome {
            if let Some(final_answer) = &outcome.final_answer {
                entry["final_answer"] = Value::String(final_answer.clone());
            }
            if let Some(wait_reason) = &outcome.wait_reason {
                entry["wait_reason"] = Value::String(wait_reason.clone());
            }
            if let Some(error) = &outcome.error {
                entry["error"] = Value::String(error.clone());
            }
            if outcome.cycles > 0 {
                entry["cycles"] = Value::Number(outcome.cycles.into());
            }
            if !outcome.todo_list.is_empty() {
                entry["todo_list"] = Value::Array(outcome.todo_list.clone());
            }
            if !outcome.resolved.is_empty() {
                entry["resolved"] = json!(outcome.resolved);
            }
        }
        if detail_level == "snapshot" {
            let recent_activity = self
                .outcome
                .as_ref()
                .and_then(|outcome| {
                    outcome
                        .final_answer
                        .clone()
                        .or_else(|| outcome.wait_reason.clone())
                        .or_else(|| outcome.error.clone())
                })
                .unwrap_or_else(|| self.task_title.clone());
            entry["snapshot"] = json!({
                "task_title": self.task_title,
                "recent_activity": recent_activity,
                "updated_at": self.updated_at,
                "workspace_files": [],
                "workspace_file_count": 0,
                "workspace_files_truncated": false,
            });
        }
        entry
    }

    fn status_label(&self) -> &'static str {
        self.outcome
            .as_ref()
            .map(|outcome| status_label(outcome.status))
            .unwrap_or_else(|| {
                if self.is_running() {
                    "running"
                } else {
                    "pending"
                }
            })
    }
}

fn status_label(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Pending => "pending",
        AgentStatus::Running => "running",
        AgentStatus::WaitUser => "wait_user",
        AgentStatus::Completed => "completed",
        AgentStatus::Failed => "failed",
        AgentStatus::MaxCycles => "max_cycles",
    }
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}
