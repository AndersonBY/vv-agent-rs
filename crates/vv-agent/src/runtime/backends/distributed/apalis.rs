use apalis::prelude::{BoxDynError, Task};
use serde::{Deserialize, Serialize};

use crate::runtime::backends::RuntimeRecipe;
use crate::types::AgentTask;

use super::CycleDispatchResult;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApalisCycleJob {
    pub task: AgentTask,
    pub recipe: RuntimeRecipe,
    pub cycle_name: String,
    pub cycle_index: u32,
}

impl ApalisCycleJob {
    pub fn new(
        task: AgentTask,
        recipe: RuntimeRecipe,
        cycle_name: impl Into<String>,
        cycle_index: u32,
    ) -> Self {
        Self {
            task,
            recipe,
            cycle_name: cycle_name.into(),
            cycle_index,
        }
    }

    pub fn from_apalis_task<Ctx, IdType>(task: Task<Self, Ctx, IdType>) -> Self {
        task.args
    }
}

pub async fn run_apalis_cycle_job<F>(
    job: ApalisCycleJob,
    cycle_handler: F,
) -> Result<CycleDispatchResult, BoxDynError>
where
    F: FnOnce(ApalisCycleJob) -> Result<CycleDispatchResult, String> + Send + 'static,
{
    cycle_handler(job).map_err(|error| BoxDynError::from(std::io::Error::other(error)))
}
