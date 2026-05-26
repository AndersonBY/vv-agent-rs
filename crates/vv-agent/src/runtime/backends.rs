use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::state::{Checkpoint, StateStore};
use super::stores::sqlite::SqliteStateStore;
use super::token_usage::summarize_task_token_usage;
use super::CancellationToken;
use crate::types::{AgentResult, AgentStatus, AgentTask, CycleRecord, Message, Metadata};

#[derive(Debug, Clone, Copy, Default)]
pub struct InlineBackend;

impl InlineBackend {
    pub fn execute<F>(
        &self,
        _task: &AgentTask,
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
        execute_cycle_loop(
            initial_messages,
            shared_state,
            cycle_executor,
            cancellation_token,
            max_cycles,
        )
    }

    pub fn parallel_map<T, R, F>(&self, function: F, items: Vec<T>) -> Vec<R>
    where
        F: Fn(T) -> R,
    {
        items.into_iter().map(function).collect()
    }
}

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

#[derive(Debug, Clone)]
pub struct ThreadBackend {
    max_workers: usize,
}

impl Default for ThreadBackend {
    fn default() -> Self {
        Self::new(4)
    }
}

impl ThreadBackend {
    pub fn new(max_workers: usize) -> Self {
        Self {
            max_workers: max_workers.max(1),
        }
    }

    pub fn max_workers(&self) -> usize {
        self.max_workers
    }

    pub fn execute<F>(
        &self,
        _task: &AgentTask,
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
        execute_cycle_loop(
            initial_messages,
            shared_state,
            cycle_executor,
            cancellation_token,
            max_cycles,
        )
    }

    pub fn submit<R, F>(&self, function: F) -> JoinHandle<R>
    where
        R: Send + 'static,
        F: FnOnce() -> R + Send + 'static,
    {
        thread::spawn(function)
    }

    pub fn parallel_map<T, R, F>(&self, function: F, items: Vec<T>) -> Vec<R>
    where
        T: Send + 'static,
        R: Send + 'static,
        F: Fn(T) -> R + Send + Sync + 'static,
    {
        let function = Arc::new(function);
        let mut indexed_items = items.into_iter().enumerate();
        let mut indexed_results = Vec::new();

        loop {
            let mut handles = Vec::new();
            for _ in 0..self.max_workers {
                let Some((index, item)) = indexed_items.next() else {
                    break;
                };
                let function = Arc::clone(&function);
                handles.push(thread::spawn(move || (index, function(item))));
            }
            if handles.is_empty() {
                break;
            }
            for handle in handles {
                indexed_results.push(handle.join().expect("thread backend worker panicked"));
            }
        }

        indexed_results.sort_by_key(|(index, _)| *index);
        indexed_results
            .into_iter()
            .map(|(_, result)| result)
            .collect()
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

#[derive(Debug, Clone, PartialEq)]
pub struct CycleTaskDispatchResult {
    pub finished: bool,
    pub result: Option<AgentResult>,
}

impl CycleTaskDispatchResult {
    pub fn unfinished() -> Self {
        Self {
            finished: false,
            result: None,
        }
    }

    pub fn finished(result: AgentResult) -> Self {
        Self {
            finished: true,
            result: Some(result),
        }
    }
}

pub trait CycleTaskDispatcher: Send + Sync {
    fn dispatch_cycle(
        &self,
        task: &AgentTask,
        recipe: &RuntimeRecipe,
        cycle_task_name: &str,
        cycle_index: u32,
    ) -> Result<CycleTaskDispatchResult, String>;
}

pub fn run_checkpointed_cycle<F>(
    state_store: &dyn StateStore,
    task: &AgentTask,
    cycle_index: u32,
    mut cycle_executor: F,
) -> Result<CycleTaskDispatchResult, String>
where
    F: FnMut(
        u32,
        &mut Vec<Message>,
        &mut Vec<CycleRecord>,
        &mut Metadata,
        Option<&CancellationToken>,
    ) -> Option<AgentResult>,
{
    let Some(mut checkpoint) = state_store
        .load_checkpoint(&task.task_id)
        .map_err(|error| error.to_string())?
    else {
        return Ok(CycleTaskDispatchResult::finished(AgentResult {
            status: AgentStatus::Failed,
            messages: Vec::new(),
            cycles: Vec::new(),
            final_answer: None,
            wait_reason: None,
            error: Some(format!("No checkpoint found for task {}", task.task_id)),
            shared_state: Metadata::new(),
            token_usage: Default::default(),
        }));
    };

    let result = cycle_executor(
        cycle_index,
        &mut checkpoint.messages,
        &mut checkpoint.cycles,
        &mut checkpoint.shared_state,
        None,
    );
    if let Some(result) = result {
        state_store
            .delete_checkpoint(&task.task_id)
            .map_err(|error| error.to_string())?;
        return Ok(CycleTaskDispatchResult::finished(result));
    }

    checkpoint.cycle_index = cycle_index;
    checkpoint.status = AgentStatus::Running;
    state_store
        .save_checkpoint(checkpoint)
        .map_err(|error| error.to_string())?;
    Ok(CycleTaskDispatchResult::unfinished())
}

#[derive(Clone)]
pub struct CeleryBackend {
    runtime_recipe: Option<RuntimeRecipe>,
    state_store: Option<Arc<dyn StateStore>>,
    cycle_dispatcher: Option<Arc<dyn CycleTaskDispatcher>>,
    cycle_task_name: String,
}

struct DistributedRunContext<'a> {
    task: &'a AgentTask,
    recipe: &'a RuntimeRecipe,
    state_store: &'a Arc<dyn StateStore>,
    cycle_dispatcher: &'a Arc<dyn CycleTaskDispatcher>,
    cancellation_token: Option<&'a CancellationToken>,
    max_cycles: u32,
}

impl std::fmt::Debug for CeleryBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CeleryBackend")
            .field("runtime_recipe", &self.runtime_recipe)
            .field("has_state_store", &self.state_store.is_some())
            .field("has_cycle_dispatcher", &self.cycle_dispatcher.is_some())
            .field("cycle_task_name", &self.cycle_task_name)
            .finish()
    }
}

impl CeleryBackend {
    pub fn inline_fallback() -> Self {
        Self {
            runtime_recipe: None,
            state_store: None,
            cycle_dispatcher: None,
            cycle_task_name: "vv_agent.celery_tasks.run_single_cycle".to_string(),
        }
    }

    pub fn distributed(runtime_recipe: RuntimeRecipe) -> Self {
        Self {
            runtime_recipe: Some(runtime_recipe),
            state_store: None,
            cycle_dispatcher: None,
            cycle_task_name: "vv_agent.celery_tasks.run_single_cycle".to_string(),
        }
    }

    pub fn distributed_with_dispatcher(
        runtime_recipe: RuntimeRecipe,
        state_store: Arc<dyn StateStore>,
        cycle_dispatcher: Arc<dyn CycleTaskDispatcher>,
    ) -> Self {
        Self {
            runtime_recipe: Some(runtime_recipe),
            state_store: Some(state_store),
            cycle_dispatcher: Some(cycle_dispatcher),
            cycle_task_name: "vv_agent.celery_tasks.run_single_cycle".to_string(),
        }
    }

    pub fn with_cycle_task_name(mut self, cycle_task_name: impl Into<String>) -> Self {
        self.cycle_task_name = cycle_task_name.into();
        self
    }

    pub fn runtime_recipe(&self) -> Option<&RuntimeRecipe> {
        self.runtime_recipe.as_ref()
    }

    pub fn state_store(&self) -> Option<&Arc<dyn StateStore>> {
        self.state_store.as_ref()
    }

    pub fn cycle_task_name(&self) -> &str {
        &self.cycle_task_name
    }

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
        if let (Some(recipe), Some(state_store), Some(cycle_dispatcher)) = (
            &self.runtime_recipe,
            &self.state_store,
            &self.cycle_dispatcher,
        ) {
            return self.execute_distributed(
                initial_messages,
                shared_state,
                DistributedRunContext {
                    task,
                    recipe,
                    state_store,
                    cycle_dispatcher,
                    cancellation_token,
                    max_cycles,
                },
            );
        }
        execute_cycle_loop(
            initial_messages,
            shared_state,
            cycle_executor,
            cancellation_token,
            max_cycles,
        )
    }

    fn execute_distributed(
        &self,
        initial_messages: Vec<Message>,
        shared_state: Metadata,
        context: DistributedRunContext<'_>,
    ) -> AgentResult {
        let checkpoint = Checkpoint {
            task_id: context.task.task_id.clone(),
            cycle_index: 0,
            status: AgentStatus::Running,
            messages: initial_messages.clone(),
            cycles: Vec::new(),
            shared_state: shared_state.clone(),
        };
        if let Err(error) = context.state_store.save_checkpoint(checkpoint) {
            return AgentResult {
                status: AgentStatus::Failed,
                messages: initial_messages,
                cycles: Vec::new(),
                final_answer: None,
                wait_reason: None,
                error: Some(format!("Failed to save initial checkpoint: {error}")),
                shared_state,
                token_usage: Default::default(),
            };
        }

        let result = self.distributed_loop(&context);
        let _ = context.state_store.delete_checkpoint(&context.task.task_id);
        result
    }

    fn distributed_loop(&self, context: &DistributedRunContext<'_>) -> AgentResult {
        for cycle_index in 1..=context.max_cycles {
            if context
                .cancellation_token
                .is_some_and(CancellationToken::is_cancelled)
            {
                let (messages, cycles, shared_state) =
                    checkpoint_snapshot(context.state_store, &context.task.task_id);
                return cancelled_backend_result(messages, cycles, shared_state);
            }

            match context.cycle_dispatcher.dispatch_cycle(
                context.task,
                context.recipe,
                &self.cycle_task_name,
                cycle_index,
            ) {
                Ok(dispatch_result) if dispatch_result.finished => {
                    return dispatch_result.result.unwrap_or_else(|| {
                        let (messages, cycles, shared_state) =
                            checkpoint_snapshot(context.state_store, &context.task.task_id);
                        failed_backend_result(
                            messages,
                            cycles,
                            shared_state,
                            format!("Celery cycle {cycle_index} finished without result payload"),
                        )
                    });
                }
                Ok(_) => {}
                Err(error) => {
                    let (messages, cycles, shared_state) =
                        checkpoint_snapshot(context.state_store, &context.task.task_id);
                    return failed_backend_result(
                        messages,
                        cycles,
                        shared_state,
                        format!("Celery cycle {cycle_index} failed: {error}"),
                    );
                }
            }
        }

        let (messages, cycles, shared_state) =
            checkpoint_snapshot(context.state_store, &context.task.task_id);
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

    pub fn parallel_map<T, R, F>(&self, function: F, items: Vec<T>) -> Vec<R>
    where
        F: Fn(T) -> R,
    {
        items.into_iter().map(function).collect()
    }
}

fn execute_cycle_loop<F>(
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

fn cancelled_backend_result(
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

fn failed_backend_result(
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

fn checkpoint_snapshot(
    state_store: &Arc<dyn StateStore>,
    task_id: &str,
) -> (Vec<Message>, Vec<CycleRecord>, Metadata) {
    match state_store.load_checkpoint(task_id) {
        Ok(Some(checkpoint)) => (
            checkpoint.messages,
            checkpoint.cycles,
            checkpoint.shared_state,
        ),
        Ok(None) | Err(_) => (Vec::new(), Vec::new(), Metadata::new()),
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
