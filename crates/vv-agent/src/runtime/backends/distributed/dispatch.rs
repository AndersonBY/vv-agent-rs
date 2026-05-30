use serde_json::Value;

use crate::runtime::backends::RuntimeRecipe;
use crate::types::{AgentResult, AgentTask};

#[derive(Debug, Clone, PartialEq)]
pub struct CycleDispatchResult {
    pub finished: bool,
    pub result: Option<AgentResult>,
}

impl CycleDispatchResult {
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

    pub fn to_dict(&self) -> Value {
        let mut payload =
            serde_json::Map::from_iter([("finished".to_string(), Value::Bool(self.finished))]);
        if let Some(result) = &self.result {
            payload.insert("result".to_string(), result.to_dict());
        }
        Value::Object(payload)
    }

    pub fn from_dict(data: &Value) -> Result<Self, String> {
        let object = data
            .as_object()
            .ok_or_else(|| "CycleDispatchResult payload must be an object".to_string())?;
        let finished = object
            .get("finished")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let result = object
            .get("result")
            .filter(|value| !value.is_null())
            .map(AgentResult::from_dict)
            .transpose()?;
        Ok(Self { finished, result })
    }
}

pub trait CycleDispatcher: Send + Sync {
    fn dispatch_cycle(
        &self,
        task: &AgentTask,
        recipe: &RuntimeRecipe,
        cycle_name: &str,
        cycle_index: u32,
    ) -> Result<CycleDispatchResult, String>;
}

pub use CycleDispatchResult as CycleTaskDispatchResult;
pub use CycleDispatcher as CycleTaskDispatcher;
