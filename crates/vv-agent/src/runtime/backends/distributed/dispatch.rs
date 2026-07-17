use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};
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
    pub committed_cycle: Option<u64>,
    pub terminal_candidate: bool,
    pub terminal_replay: bool,
}

impl Serialize for CycleDispatchResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.to_dict().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CycleDispatchResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        Self::from_dict(&value).map_err(D::Error::custom)
    }
}

impl CycleDispatchResult {
    pub fn unfinished() -> Self {
        Self {
            finished: false,
            result: None,
            checkpoint_revision: None,
            committed_cycle: None,
            terminal_candidate: false,
            terminal_replay: false,
        }
    }

    pub fn committed(cycle_index: u64, checkpoint_revision: u64) -> Self {
        Self {
            finished: false,
            result: None,
            checkpoint_revision: Some(checkpoint_revision),
            committed_cycle: Some(cycle_index),
            terminal_candidate: false,
            terminal_replay: false,
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
            committed_cycle: None,
            terminal_candidate: false,
            terminal_replay: false,
        }
    }

    pub fn terminal_candidate(result: AgentResult, checkpoint_revision: u64) -> Self {
        Self {
            finished: true,
            result: Some(result),
            checkpoint_revision: Some(checkpoint_revision),
            committed_cycle: None,
            terminal_candidate: true,
            terminal_replay: false,
        }
    }

    pub fn terminal_replay(result: AgentResult, checkpoint_revision: u64) -> Self {
        Self {
            finished: true,
            result: Some(result),
            checkpoint_revision: Some(checkpoint_revision),
            committed_cycle: None,
            terminal_candidate: false,
            terminal_replay: true,
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
        if let Some(cycle_index) = self.committed_cycle {
            payload.insert("committed_cycle".to_string(), Value::from(cycle_index));
        }
        if self.terminal_candidate {
            payload.insert("terminal_candidate".to_string(), Value::Bool(true));
        }
        if self.terminal_replay {
            payload.insert("terminal_replay".to_string(), Value::Bool(true));
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
        let committed_cycle = match object.get("committed_cycle") {
            None | Some(Value::Null) => None,
            Some(value) => Some(value.as_u64().ok_or_else(|| {
                "CycleDispatchResult committed_cycle must be an unsigned integer".to_string()
            })?),
        };
        let terminal_candidate = optional_bool(object, "terminal_candidate")?;
        let terminal_replay = optional_bool(object, "terminal_replay")?;
        let result = Self {
            finished,
            result,
            checkpoint_revision,
            committed_cycle,
            terminal_candidate,
            terminal_replay,
        };
        result.validate()?;
        Ok(result)
    }

    fn validate(&self) -> Result<(), String> {
        if self.terminal_candidate && self.terminal_replay {
            return Err(
                "CycleDispatchResult terminal_candidate and terminal_replay are mutually exclusive"
                    .to_string(),
            );
        }
        if (self.terminal_candidate || self.terminal_replay)
            && (!self.finished || self.result.is_none() || self.checkpoint_revision.is_none())
        {
            return Err(
                "CycleDispatchResult terminal disposition requires a finished result and checkpoint_revision"
                    .to_string(),
            );
        }
        if self.committed_cycle.is_some()
            && (self.finished
                || self.result.is_some()
                || self.terminal_candidate
                || self.terminal_replay)
        {
            return Err(
                "CycleDispatchResult committed_cycle is only valid for unfinished progress"
                    .to_string(),
            );
        }
        Ok(())
    }
}

fn optional_bool(object: &serde_json::Map<String, Value>, field: &str) -> Result<bool, String> {
    match object.get(field) {
        None => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("CycleDispatchResult {field} must be a boolean")),
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
