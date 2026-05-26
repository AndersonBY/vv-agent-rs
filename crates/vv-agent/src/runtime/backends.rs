use std::sync::Arc;
use std::thread::{self, JoinHandle};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default)]
pub struct InlineBackend;

impl InlineBackend {
    pub fn parallel_map<T, R, F>(&self, function: F, items: Vec<T>) -> Vec<R>
    where
        F: Fn(T) -> R,
    {
        items.into_iter().map(function).collect()
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
}

#[derive(Debug, Clone)]
pub struct CeleryBackend {
    runtime_recipe: Option<RuntimeRecipe>,
    cycle_task_name: String,
}

impl CeleryBackend {
    pub fn inline_fallback() -> Self {
        Self {
            runtime_recipe: None,
            cycle_task_name: "vv_agent.celery_tasks.run_single_cycle".to_string(),
        }
    }

    pub fn distributed(runtime_recipe: RuntimeRecipe) -> Self {
        Self {
            runtime_recipe: Some(runtime_recipe),
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

    pub fn cycle_task_name(&self) -> &str {
        &self.cycle_task_name
    }

    pub fn parallel_map<T, R, F>(&self, function: F, items: Vec<T>) -> Vec<R>
    where
        F: Fn(T) -> R,
    {
        items.into_iter().map(function).collect()
    }
}
