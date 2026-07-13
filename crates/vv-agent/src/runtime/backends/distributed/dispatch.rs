use serde_json::Value;

use crate::runtime::backends::RuntimeRecipe;
use crate::runtime::CancellationToken;
use crate::types::{AgentResult, AgentTask};

use super::DistributedRunEnvelope;

#[derive(Debug, Clone, PartialEq)]
pub struct CycleDispatchResult {
    pub finished: bool,
    pub result: Option<AgentResult>,
    pub checkpoint_revision: Option<u64>,
}

impl CycleDispatchResult {
    pub fn unfinished() -> Self {
        Self {
            finished: false,
            result: None,
            checkpoint_revision: None,
        }
    }

    pub fn finished(result: AgentResult) -> Self {
        Self::finished_at_revision(result, None)
    }

    pub fn finished_at_revision(result: AgentResult, checkpoint_revision: Option<u64>) -> Self {
        Self {
            finished: true,
            result: Some(result),
            checkpoint_revision,
        }
    }

    pub fn to_dict(&self) -> Value {
        let mut payload =
            serde_json::Map::from_iter([("finished".to_string(), Value::Bool(self.finished))]);
        if let Some(result) = &self.result {
            payload.insert("result".to_string(), result.to_dict());
        }
        if let Some(revision) = self.checkpoint_revision {
            payload.insert("checkpoint_revision".to_string(), Value::from(revision));
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
            .ok_or_else(|| "CycleDispatchResult finished must be a boolean".to_string())?;
        let result = object
            .get("result")
            .filter(|value| !value.is_null())
            .map(AgentResult::from_dict)
            .transpose()?;
        if finished != result.is_some() {
            return Err(
                "CycleDispatchResult result must be present exactly when finished is true"
                    .to_string(),
            );
        }
        let checkpoint_revision = match object.get("checkpoint_revision") {
            None | Some(Value::Null) => None,
            Some(value) => Some(value.as_u64().ok_or_else(|| {
                "CycleDispatchResult checkpoint_revision must be an unsigned integer".to_string()
            })?),
        };
        Ok(Self {
            finished,
            result,
            checkpoint_revision,
        })
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

    fn dispatch_envelope(
        &self,
        envelope: &DistributedRunEnvelope,
    ) -> Result<CycleDispatchResult, String> {
        self.dispatch_cycle(
            &envelope.task,
            &envelope.recipe,
            &envelope.cycle_name,
            envelope.cycle_index,
        )
    }

    fn dispatch_envelope_with_cancellation(
        &self,
        envelope: &DistributedRunEnvelope,
        cancellation_token: Option<&CancellationToken>,
    ) -> Result<CycleDispatchResult, String> {
        check_cancellation(cancellation_token)?;
        let result = self.dispatch_envelope(envelope)?;
        if result.finished {
            return Ok(result);
        }
        check_cancellation(cancellation_token)?;
        Ok(result)
    }
}

fn check_cancellation(cancellation_token: Option<&CancellationToken>) -> Result<(), String> {
    cancellation_token
        .map(CancellationToken::check)
        .transpose()
        .map(|_| ())
        .map_err(|reason| format!("distributed dispatch cancelled: {reason}"))
}
