pub mod base;
pub mod celery;
pub mod celery_tasks;
pub mod inline;
pub mod thread;

use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::stores::sqlite::SqliteStateStore;
use super::token_usage::summarize_task_token_usage;
use super::CancellationToken;
use crate::types::{AgentResult, AgentStatus, AgentTask, CycleRecord, Message, Metadata};

pub use celery::{CeleryBackend, CycleTaskDispatchResult, CycleTaskDispatcher};
pub use celery_tasks::run_checkpointed_cycle;
pub use inline::InlineBackend;
pub use thread::ThreadBackend;
pub use RuntimeExecutionBackend as ExecutionBackend;

#[derive(Debug, Clone)]
pub enum RuntimeExecutionBackend {
    Inline(InlineBackend),
    Thread(ThreadBackend),
    Celery(CeleryBackend),
}

impl Default for RuntimeExecutionBackend {
    fn default() -> Self {
        Self::Inline(InlineBackend)
    }
}

impl From<InlineBackend> for RuntimeExecutionBackend {
    fn from(backend: InlineBackend) -> Self {
        Self::Inline(backend)
    }
}

impl From<ThreadBackend> for RuntimeExecutionBackend {
    fn from(backend: ThreadBackend) -> Self {
        Self::Thread(backend)
    }
}

impl From<CeleryBackend> for RuntimeExecutionBackend {
    fn from(backend: CeleryBackend) -> Self {
        Self::Celery(backend)
    }
}

impl RuntimeExecutionBackend {
    pub fn execute<F>(
        &self,
        task: &AgentTask,
        initial_messages: Vec<Message>,
        shared_state: Metadata,
        cycle_executor: F,
        cancellation_token: Option<&CancellationToken>,
        max_cycles: u32,
    ) -> AgentResult
    where
        F: FnMut(
            u32,
            &mut Vec<Message>,
            &mut Vec<CycleRecord>,
            &mut Metadata,
            Option<&CancellationToken>,
        ) -> Option<AgentResult>,
    {
        match self {
            Self::Inline(backend) => backend.execute(
                task,
                initial_messages,
                shared_state,
                cycle_executor,
                cancellation_token,
                max_cycles,
            ),
            Self::Thread(backend) => backend.execute(
                task,
                initial_messages,
                shared_state,
                cycle_executor,
                cancellation_token,
                max_cycles,
            ),
            Self::Celery(backend) => backend.execute(
                task,
                initial_messages,
                shared_state,
                cycle_executor,
                cancellation_token,
                max_cycles,
            ),
        }
    }

    pub fn parallel_map<T, R, F>(&self, function: F, items: Vec<T>) -> Vec<R>
    where
        T: Send + 'static,
        R: Send + 'static,
        F: Fn(T) -> R + Send + Sync + 'static,
    {
        match self {
            Self::Inline(backend) => backend.parallel_map(function, items),
            Self::Thread(backend) => backend.parallel_map(function, items),
            Self::Celery(backend) => backend.parallel_map(function, items),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeRecipe {
    pub settings_file: String,
    pub backend: String,
    pub model: String,
    pub workspace: String,
    pub timeout_seconds: f64,
    pub hook_class_paths: Vec<String>,
    pub log_preview_chars: Option<usize>,
}

impl RuntimeRecipe {
    pub fn new(
        settings_file: impl Into<String>,
        backend: impl Into<String>,
        model: impl Into<String>,
        workspace: impl Into<String>,
    ) -> Self {
        Self {
            settings_file: settings_file.into(),
            backend: backend.into(),
            model: model.into(),
            workspace: workspace.into(),
            timeout_seconds: 90.0,
            hook_class_paths: Vec::new(),
            log_preview_chars: None,
        }
    }

    pub fn to_dict(&self) -> Value {
        serde_json::json!({
            "settings_file": self.settings_file,
            "backend": self.backend,
            "model": self.model,
            "workspace": self.workspace,
            "timeout_seconds": self.timeout_seconds,
            "hook_class_paths": self.hook_class_paths,
            "log_preview_chars": self.log_preview_chars,
        })
    }

    pub fn from_dict(data: &Value) -> Result<Self, String> {
        let object = data
            .as_object()
            .ok_or_else(|| "RuntimeRecipe payload must be an object".to_string())?;
        Ok(Self {
            settings_file: read_required_string(object, "settings_file")?.to_string(),
            backend: read_required_string(object, "backend")?.to_string(),
            model: read_required_string(object, "model")?.to_string(),
            workspace: read_required_string(object, "workspace")?.to_string(),
            timeout_seconds: object
                .get("timeout_seconds")
                .and_then(Value::as_f64)
                .unwrap_or(90.0),
            hook_class_paths: object
                .get("hook_class_paths")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            log_preview_chars: object
                .get("log_preview_chars")
                .filter(|value| !value.is_null())
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok()),
        })
    }

    pub fn default_sqlite_checkpoint_path(&self) -> PathBuf {
        PathBuf::from(&self.workspace)
            .join(".vv-agent-state")
            .join("checkpoints.db")
    }

    pub fn build_default_state_store(&self) -> io::Result<SqliteStateStore> {
        let db_path = self.default_sqlite_checkpoint_path();
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        SqliteStateStore::new(db_path)
    }
}

pub(super) fn execute_cycle_loop<F>(
    mut messages: Vec<Message>,
    mut shared_state: Metadata,
    mut cycle_executor: F,
    cancellation_token: Option<&CancellationToken>,
    max_cycles: u32,
) -> AgentResult
where
    F: FnMut(
        u32,
        &mut Vec<Message>,
        &mut Vec<CycleRecord>,
        &mut Metadata,
        Option<&CancellationToken>,
    ) -> Option<AgentResult>,
{
    let mut cycles = Vec::new();

    for cycle_index in 1..=max_cycles {
        if cancellation_token.is_some_and(CancellationToken::is_cancelled) {
            return cancelled_backend_result(messages, cycles, shared_state);
        }
        if let Some(result) = cycle_executor(
            cycle_index,
            &mut messages,
            &mut cycles,
            &mut shared_state,
            cancellation_token,
        ) {
            return result;
        }
    }

    let token_usage = summarize_task_token_usage(&cycles);
    AgentResult {
        status: AgentStatus::MaxCycles,
        messages,
        cycles,
        final_answer: Some("Reached max cycles without finish signal.".to_string()),
        wait_reason: None,
        error: None,
        shared_state,
        token_usage,
    }
}

pub(super) fn cancelled_backend_result(
    messages: Vec<Message>,
    cycles: Vec<CycleRecord>,
    shared_state: Metadata,
) -> AgentResult {
    let token_usage = summarize_task_token_usage(&cycles);
    AgentResult {
        status: AgentStatus::Failed,
        messages,
        cycles,
        final_answer: None,
        wait_reason: None,
        error: Some("Operation was cancelled".to_string()),
        shared_state,
        token_usage,
    }
}

pub(super) fn failed_backend_result(
    messages: Vec<Message>,
    cycles: Vec<CycleRecord>,
    shared_state: Metadata,
    error: String,
) -> AgentResult {
    let token_usage = summarize_task_token_usage(&cycles);
    AgentResult {
        status: AgentStatus::Failed,
        messages,
        cycles,
        final_answer: None,
        wait_reason: None,
        error: Some(error),
        shared_state,
        token_usage,
    }
}

fn read_required_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing required string field {key:?}"))
}
