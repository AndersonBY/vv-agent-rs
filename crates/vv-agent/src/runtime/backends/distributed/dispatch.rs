use serde::{de::Error as _, ser::Error as _, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::runtime::CancellationToken;
use crate::types::{AgentResult, AgentStatus};

use super::DistributedRunEnvelope;

pub const DISTRIBUTED_WORKER_RESPONSE_SCHEMA_VERSION: &str =
    "vv-agent.distributed-worker-response.v1";
const JSON_SAFE_INTEGER_MAX: u64 = (1_u64 << 53) - 1;
const INVALID_AGENT_RESULT: &str =
    "distributed worker response result must be a complete current AgentResult";

#[derive(Debug, Clone, PartialEq)]
pub enum CycleDispatchResult {
    Pending,
    Committed {
        checkpoint_revision: u64,
        committed_cycle: u64,
    },
    TerminalCandidate {
        checkpoint_revision: u64,
        result: AgentResult,
    },
    TerminalReplay {
        checkpoint_revision: u64,
        result: AgentResult,
    },
}

impl Serialize for CycleDispatchResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(S::Error::custom)?;
        self.wire_value().serialize(serializer)
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
    pub const fn pending() -> Self {
        Self::Pending
    }

    pub fn committed(cycle_index: u64, checkpoint_revision: u64) -> Result<Self, String> {
        let result = Self::Committed {
            checkpoint_revision,
            committed_cycle: cycle_index,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn terminal_candidate(
        result: AgentResult,
        checkpoint_revision: u64,
    ) -> Result<Self, String> {
        let result = Self::TerminalCandidate {
            checkpoint_revision,
            result,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn terminal_replay(result: AgentResult, checkpoint_revision: u64) -> Result<Self, String> {
        let result = Self::TerminalReplay {
            checkpoint_revision,
            result,
        };
        result.validate()?;
        Ok(result)
    }

    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Committed { .. } => "committed",
            Self::TerminalCandidate { .. } => "terminal_candidate",
            Self::TerminalReplay { .. } => "terminal_replay",
        }
    }

    pub fn to_dict(&self) -> Value {
        self.validate()
            .expect("CycleDispatchResult must satisfy the distributed worker response contract");
        self.wire_value()
    }

    pub fn from_dict(data: &Value) -> Result<Self, String> {
        let object = data
            .as_object()
            .ok_or_else(|| "distributed worker response must be an object".to_string())?;
        if object.get("schema_version").and_then(Value::as_str)
            != Some(DISTRIBUTED_WORKER_RESPONSE_SCHEMA_VERSION)
        {
            return Err("unsupported distributed worker response schema_version".to_string());
        }
        let response_type = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| "unsupported distributed worker response type".to_string())?;

        match response_type {
            "pending" => {
                require_exact_fields(object, response_type, &["schema_version", "type"])?;
                Ok(Self::Pending)
            }
            "committed" => {
                require_exact_fields(
                    object,
                    response_type,
                    &[
                        "schema_version",
                        "type",
                        "checkpoint_revision",
                        "committed_cycle",
                    ],
                )?;
                Ok(Self::Committed {
                    checkpoint_revision: read_wire_integer(object, "checkpoint_revision")?,
                    committed_cycle: read_positive_wire_integer(object, "committed_cycle")?,
                })
            }
            "terminal_candidate" | "terminal_replay" => {
                require_exact_fields(
                    object,
                    response_type,
                    &["schema_version", "type", "checkpoint_revision", "result"],
                )?;
                let checkpoint_revision = read_wire_integer(object, "checkpoint_revision")?;
                let result = parse_complete_agent_result(
                    object.get("result").expect("exact fields checked above"),
                )?;
                if response_type == "terminal_candidate" {
                    validate_terminal_candidate_result(&result)?;
                    Ok(Self::TerminalCandidate {
                        checkpoint_revision,
                        result,
                    })
                } else {
                    validate_terminal_replay_result(&result)?;
                    Ok(Self::TerminalReplay {
                        checkpoint_revision,
                        result,
                    })
                }
            }
            _ => Err("unsupported distributed worker response type".to_string()),
        }
    }

    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Pending => Ok(()),
            Self::Committed {
                checkpoint_revision,
                committed_cycle,
            } => {
                validate_wire_integer(*checkpoint_revision, "checkpoint_revision")?;
                validate_positive_wire_integer(*committed_cycle, "committed_cycle")
            }
            Self::TerminalCandidate {
                checkpoint_revision,
                result,
            } => {
                validate_wire_integer(*checkpoint_revision, "checkpoint_revision")?;
                validate_terminal_candidate_result(result)
            }
            Self::TerminalReplay {
                checkpoint_revision,
                result,
            } => {
                validate_wire_integer(*checkpoint_revision, "checkpoint_revision")?;
                validate_terminal_replay_result(result)
            }
        }
    }

    fn wire_value(&self) -> Value {
        let mut payload = Map::from_iter([
            (
                "schema_version".to_string(),
                Value::String(DISTRIBUTED_WORKER_RESPONSE_SCHEMA_VERSION.to_string()),
            ),
            ("type".to_string(), Value::String(self.kind().to_string())),
        ]);
        match self {
            Self::Pending => {}
            Self::Committed {
                checkpoint_revision,
                committed_cycle,
            } => {
                payload.insert(
                    "checkpoint_revision".to_string(),
                    Value::from(*checkpoint_revision),
                );
                payload.insert("committed_cycle".to_string(), Value::from(*committed_cycle));
            }
            Self::TerminalCandidate {
                checkpoint_revision,
                result,
            }
            | Self::TerminalReplay {
                checkpoint_revision,
                result,
            } => {
                payload.insert(
                    "checkpoint_revision".to_string(),
                    Value::from(*checkpoint_revision),
                );
                payload.insert("result".to_string(), result.to_dict());
            }
        }
        Value::Object(payload)
    }
}

fn require_exact_fields(
    object: &Map<String, Value>,
    response_type: &str,
    expected: &[&str],
) -> Result<(), String> {
    if object.len() != expected.len() || expected.iter().any(|field| !object.contains_key(*field)) {
        return Err(format!(
            "distributed worker response fields do not match type {response_type}"
        ));
    }
    Ok(())
}

fn read_wire_integer(object: &Map<String, Value>, field: &str) -> Result<u64, String> {
    let value = object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{field} must be a JSON-safe unsigned integer"))?;
    validate_wire_integer(value, field)?;
    Ok(value)
}

fn validate_wire_integer(value: u64, field: &str) -> Result<(), String> {
    if value > JSON_SAFE_INTEGER_MAX {
        return Err(format!("{field} must be a JSON-safe unsigned integer"));
    }
    Ok(())
}

fn read_positive_wire_integer(object: &Map<String, Value>, field: &str) -> Result<u64, String> {
    let value = read_wire_integer(object, field)?;
    validate_positive_wire_integer(value, field)?;
    Ok(value)
}

fn validate_positive_wire_integer(value: u64, field: &str) -> Result<(), String> {
    validate_wire_integer(value, field)?;
    if value == 0 {
        return Err(format!("{field} must be a positive JSON-safe integer"));
    }
    Ok(())
}

fn parse_complete_agent_result(value: &Value) -> Result<AgentResult, String> {
    let result = AgentResult::from_dict(value).map_err(|_| invalid_agent_result())?;
    if result.to_dict() != *value {
        return Err(invalid_agent_result());
    }
    Ok(result)
}

fn validate_terminal_candidate_result(result: &AgentResult) -> Result<(), String> {
    if matches!(result.status, AgentStatus::Pending | AgentStatus::Running) {
        return Err(invalid_agent_result());
    }
    Ok(())
}

fn validate_terminal_replay_result(result: &AgentResult) -> Result<(), String> {
    if matches!(
        result.status,
        AgentStatus::Pending | AgentStatus::Running | AgentStatus::ReconciliationRequired
    ) {
        return Err(invalid_agent_result());
    }
    Ok(())
}

fn invalid_agent_result() -> String {
    INVALID_AGENT_RESULT.to_string()
}

pub trait CycleDispatcher: Send + Sync {
    fn dispatch_envelope(
        &self,
        envelope: &DistributedRunEnvelope,
    ) -> Result<CycleDispatchResult, String>;

    fn dispatch_envelope_with_cancellation(
        &self,
        envelope: &DistributedRunEnvelope,
        cancellation_token: Option<&CancellationToken>,
    ) -> Result<CycleDispatchResult, String> {
        check_cancellation(cancellation_token)?;
        let result = self.dispatch_envelope(envelope)?;
        if matches!(
            &result,
            CycleDispatchResult::TerminalCandidate { .. }
                | CycleDispatchResult::TerminalReplay { .. }
        ) {
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
