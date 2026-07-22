use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::budget::{BudgetUsageSnapshot, HostCostMeter, RunBudgetLimits};
use crate::events::RunEvent;
use crate::model::ModelProvider;
use crate::runtime::cancellation::CancellationToken;
use crate::runtime::checkpoint_resume::CheckpointController;
use crate::runtime::context::ExecutionContext;
use crate::runtime::sub_task_manager::SubTaskManager;
use crate::types::{CycleRecord, Message};
use crate::workspace::WorkspaceBackend;
use crate::{RunConfig, RunContext};

pub type RunEventHandler = Arc<dyn Fn(&RunEvent) + Send + Sync + 'static>;
pub type BeforeCycleMessageProvider =
    Arc<dyn Fn(u32, &[Message], &BTreeMap<String, Value>) -> Vec<Message> + Send + Sync + 'static>;
pub type InterruptionMessageProvider = Arc<dyn Fn() -> Vec<Message> + Send + Sync + 'static>;

#[doc(hidden)]
#[derive(Clone)]
pub struct CheckpointRuntimeControl {
    controller: CheckpointController,
}

impl CheckpointRuntimeControl {
    pub(crate) fn new(controller: CheckpointController) -> Self {
        Self { controller }
    }

    pub(crate) fn controller(&self) -> &CheckpointController {
        &self.controller
    }

    pub(crate) fn into_controller(self) -> CheckpointController {
        self.controller
    }
}

#[derive(Clone, Default)]
pub struct RuntimeRunControls {
    pub event_handler: Option<RunEventHandler>,
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
    pub budget_limits: Option<RunBudgetLimits>,
    pub host_cost_meter: Option<Arc<dyn HostCostMeter>>,
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
    #[doc(hidden)]
    pub initial_budget_usage: Option<BudgetUsageSnapshot>,
    #[doc(hidden)]
    pub defer_terminal_on_max_cycles: bool,
    #[doc(hidden)]
    pub checkpoint_controller: Option<CheckpointRuntimeControl>,
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

    pub(in crate::runtime::engine) fn effective_event_handler(&self) -> Option<RunEventHandler> {
        self.execution_context
            .as_ref()
            .and_then(|context| context.event_handler.clone())
            .or_else(|| self.event_handler.clone())
    }

    pub(crate) fn effective_checkpoint_controller(&self) -> Option<&CheckpointController> {
        self.checkpoint_controller
            .as_ref()
            .map(CheckpointRuntimeControl::controller)
    }
}
