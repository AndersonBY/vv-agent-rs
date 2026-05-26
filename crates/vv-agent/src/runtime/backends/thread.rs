use std::sync::Arc;
use std::thread::{self, JoinHandle};

use super::execute_cycle_loop;
use crate::runtime::CancellationToken;
use crate::types::{AgentResult, AgentTask, CycleRecord, Message, Metadata};

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
