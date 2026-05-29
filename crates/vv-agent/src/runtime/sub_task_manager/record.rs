use std::collections::BTreeMap;
use std::sync::Arc;
use std::thread::JoinHandle;

use serde_json::{json, Value};

use crate::runtime::sub_agent_sessions::SubAgentSession;
use crate::types::SubTaskOutcome;
use crate::workspace::WorkspaceBackend;

use super::helpers::{is_internal_workspace_file, preview_text, status_label};
use super::types::ManagedSubTaskSnapshot;

pub struct ManagedSubTask {
    pub(super) task_id: String,
    pub(super) session_id: String,
    pub(super) agent_name: String,
    pub(super) task_title: String,
    pub(super) workspace_backend: Option<Arc<dyn WorkspaceBackend>>,
    pub(super) session: Option<Arc<dyn SubAgentSession>>,
    pub(super) outcome: Option<SubTaskOutcome>,
    pub(super) resolved: BTreeMap<String, String>,
    pub(super) current_cycle_index: Option<u32>,
    pub(super) recent_activity: Option<String>,
    pub(super) latest_cycle: Option<Value>,
    pub(super) latest_tool_call: Option<Value>,
    pub(super) handle: Option<JoinHandle<()>>,
    pub(super) updated_at: String,
    pub(super) manager_listener_attached: bool,
}

impl ManagedSubTask {
    pub(super) fn is_running(&self) -> bool {
        self.handle
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
    }

    pub(super) fn to_status_entry(&self, detail_level: &str, workspace_file_limit: usize) -> Value {
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
                .recent_activity
                .clone()
                .or_else(|| {
                    self.outcome.as_ref().and_then(|outcome| {
                        outcome
                            .final_answer
                            .clone()
                            .or_else(|| outcome.wait_reason.clone())
                            .or_else(|| outcome.error.clone())
                    })
                })
                .unwrap_or_else(|| self.task_title.clone());
            let workspace_snapshot = self.workspace_snapshot(workspace_file_limit);
            entry["snapshot"] = json!({
                "current_cycle_index": self.current_cycle_index,
                "task_title": self.task_title,
                "recent_activity": recent_activity,
                "updated_at": self.updated_at,
                "workspace_files": workspace_snapshot.files,
                "workspace_file_count": workspace_snapshot.file_count,
                "workspace_files_truncated": workspace_snapshot.truncated,
            });
            if let Some(latest_cycle) = &self.latest_cycle {
                entry["snapshot"]["latest_cycle"] = latest_cycle.clone();
            }
            if let Some(latest_tool_call) = &self.latest_tool_call {
                entry["snapshot"]["latest_tool_call"] = latest_tool_call.clone();
            }
        }
        entry
    }

    pub(super) fn snapshot(&self) -> ManagedSubTaskSnapshot {
        ManagedSubTaskSnapshot {
            task_id: self.task_id.clone(),
            session_id: self.session_id.clone(),
            agent_name: self.agent_name.clone(),
            task_title: self.task_title.clone(),
            status: self.status_label().to_string(),
            running: self.is_running(),
            outcome: self.outcome.clone(),
            resolved: self.resolved.clone(),
            current_cycle_index: self.current_cycle_index,
            recent_activity: self.recent_activity.clone(),
            latest_cycle: self.latest_cycle.clone(),
            latest_tool_call: self.latest_tool_call.clone(),
            updated_at: self.updated_at.clone(),
        }
    }

    pub(super) fn status_label(&self) -> &'static str {
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

    pub(super) fn update_from_outcome(&mut self, outcome: &SubTaskOutcome) {
        if outcome.cycles > 0 {
            self.current_cycle_index = Some(outcome.cycles);
        }
        self.set_latest_cycle_status(status_label(outcome.status));
        if let Some(detail) = outcome
            .final_answer
            .as_ref()
            .or(outcome.wait_reason.as_ref())
            .or(outcome.error.as_ref())
            .and_then(|value| preview_text(Some(&Value::String(value.clone()))))
        {
            self.recent_activity = Some(detail);
        }
    }

    pub(super) fn set_latest_cycle_status(&mut self, status: &str) {
        let mut latest_cycle = self
            .latest_cycle
            .as_ref()
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        latest_cycle.insert("status".to_string(), Value::String(status.to_string()));
        if let Some(current_cycle_index) = self.current_cycle_index {
            latest_cycle
                .entry("cycle_index".to_string())
                .or_insert_with(|| Value::from(current_cycle_index));
        }
        self.latest_cycle = Some(Value::Object(latest_cycle));
    }

    pub(super) fn mark_terminal_state(&mut self, status: &str, detail: Option<&str>) {
        self.set_latest_cycle_status(status);
        if let Some(detail) = detail {
            self.recent_activity = Some(detail.to_string());
        }
    }

    fn workspace_snapshot(&self, workspace_file_limit: usize) -> WorkspaceSnapshot {
        let Some(workspace_backend) = &self.workspace_backend else {
            return WorkspaceSnapshot::default();
        };
        let Ok(raw_files) = workspace_backend.list_files(".", "**/*") else {
            return WorkspaceSnapshot::default();
        };
        let visible_files = raw_files
            .into_iter()
            .filter(|path| !is_internal_workspace_file(path))
            .collect::<Vec<_>>();
        let file_count = visible_files.len();
        WorkspaceSnapshot {
            files: visible_files
                .into_iter()
                .take(workspace_file_limit)
                .collect::<Vec<_>>(),
            file_count,
            truncated: file_count > workspace_file_limit,
        }
    }
}

#[derive(Default)]
struct WorkspaceSnapshot {
    files: Vec<String>,
    file_count: usize,
    truncated: bool,
}
