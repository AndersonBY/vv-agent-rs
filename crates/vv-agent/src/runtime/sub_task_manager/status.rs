use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::manager::SubTaskManager;
use super::record::ManagedSubTask;
use super::types::ManagedSubTaskSnapshot;

impl SubTaskManager {
    pub fn status_entries(
        &self,
        task_ids: &[String],
        detail_level: &str,
        workspace_file_limit: usize,
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
                record.to_status_entry(detail_level, workspace_file_limit)
            })
            .collect()
    }

    pub fn get(&self, task_id: &str) -> Option<ManagedSubTaskSnapshot> {
        self.join_finished_tasks();
        self.tasks
            .lock()
            .expect("sub-task manager poisoned")
            .get(task_id)
            .map(ManagedSubTask::snapshot)
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

    pub fn wait(&self, task_id: &str, timeout: Option<Duration>) -> bool {
        let deadline = timeout.map(|duration| Instant::now() + duration);
        loop {
            let handle = {
                let mut tasks = self.tasks.lock().expect("sub-task manager poisoned");
                let Some(record) = tasks.get_mut(task_id) else {
                    return false;
                };
                match record.handle.as_ref() {
                    Some(handle) if timeout.is_none() || handle.is_finished() => {
                        record.handle.take()
                    }
                    Some(_) => None,
                    None => return true,
                }
            };

            if let Some(handle) = handle {
                let _ = handle.join();
                return true;
            }

            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return false;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn wait_for_record(
        &self,
        task_id: &str,
        timeout: Option<Duration>,
    ) -> Option<ManagedSubTaskSnapshot> {
        if !self.wait(task_id, timeout) {
            return None;
        }
        self.get(task_id)
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
