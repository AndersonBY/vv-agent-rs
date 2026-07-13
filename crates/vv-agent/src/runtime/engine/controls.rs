use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::model::ModelProvider;
use crate::runtime::cancellation::CancellationToken;
use crate::runtime::context::{ExecutionContext, StreamCallback};
use crate::runtime::sub_task_manager::SubTaskManager;
use crate::types::{CycleRecord, Message};
use crate::workspace::WorkspaceBackend;
use crate::{RunConfig, RunContext};

pub type RuntimeLogCallback = dyn FnMut(&str, &BTreeMap<String, Value>) + Send + Sync + 'static;
pub type RuntimeLogHandler = Arc<Mutex<Box<RuntimeLogCallback>>>;
pub type RuntimeEventHandler = Arc<dyn Fn(&str, &BTreeMap<String, Value>) + Send + Sync + 'static>;
pub type BeforeCycleMessageProvider =
    Arc<dyn Fn(u32, &[Message], &BTreeMap<String, Value>) -> Vec<Message> + Send + Sync + 'static>;
pub type InterruptionMessageProvider = Arc<dyn Fn() -> Vec<Message> + Send + Sync + 'static>;

#[derive(Clone, Default)]
pub struct RuntimeRunControls {
    pub log_handler: Option<RuntimeEventHandler>,
    pub before_cycle_messages: Option<BeforeCycleMessageProvider>,
    pub interruption_messages: Option<InterruptionMessageProvider>,
    pub steering_queue: Option<Arc<Mutex<VecDeque<String>>>>,
    pub cancellation_token: Option<CancellationToken>,
    pub execution_context: Option<ExecutionContext>,
    pub workspace: Option<PathBuf>,
    pub workspace_backend: Option<Arc<dyn WorkspaceBackend>>,
    pub model_provider: Option<Arc<dyn ModelProvider>>,
    pub run_context: Option<RunContext>,
    pub sub_task_manager: Option<SubTaskManager>,
    #[doc(hidden)]
    pub background_parent_run_config: Option<RunConfig>,
    #[doc(hidden)]
    pub initial_messages: Option<Vec<Message>>,
    #[doc(hidden)]
    pub initial_shared_state: Option<BTreeMap<String, Value>>,
    #[doc(hidden)]
    pub initial_cycles: Option<Vec<CycleRecord>>,
    #[doc(hidden)]
    pub cycle_index_start: Option<u32>,
    #[doc(hidden)]
    pub cycle_count: Option<u32>,
}

impl RuntimeRunControls {
    pub(in crate::runtime::engine) fn effective_cancellation_token(
        &self,
    ) -> Option<CancellationToken> {
        self.cancellation_token.clone().or_else(|| {
            self.execution_context
                .as_ref()
                .and_then(|context| context.cancellation_token.clone())
        })
    }

    pub(in crate::runtime::engine) fn effective_stream_callback(&self) -> Option<StreamCallback> {
        self.execution_context
            .as_ref()
            .and_then(|context| context.stream_callback.clone())
    }
}
