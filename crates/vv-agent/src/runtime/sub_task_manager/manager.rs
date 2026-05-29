use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::record::ManagedSubTask;

#[derive(Clone, Default)]
pub struct SubTaskManager {
    pub(super) tasks: Arc<Mutex<BTreeMap<String, ManagedSubTask>>>,
}

impl std::fmt::Debug for SubTaskManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SubTaskManager")
            .finish_non_exhaustive()
    }
}
