use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::AgentTask;

use super::super::RuntimeRecipe;

pub const DISTRIBUTED_RUN_SCHEMA_VERSION: &str = "vv-agent.distributed-run.v1";
pub const DEFAULT_TOOLSET_ID: &str = "vv-agent.builtin-tools";
pub const DEFAULT_TOOLSET_VERSION: &str = "1";
pub const DEFAULT_TOOLSET_SCHEMA_DIGEST: &str =
    "f85422117d41d28ffa3cdfcfd9a42892854de624808fadc2124f4ebe7a452b61";
pub const DEFAULT_CYCLE_NAME: &str = "vv_agent.distributed.run_single_cycle";
pub const DEFAULT_LEASE_DURATION_MS: u64 = 5 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CapabilityRef {
    pub id: String,
    pub version: String,
}

impl CapabilityRef {
    pub fn new(id: impl Into<String>, version: impl Into<String>) -> Result<Self, String> {
        let reference = Self {
            id: id.into(),
            version: version.into(),
        };
        reference.validate("capability_ref")?;
        Ok(reference)
    }

    pub fn validate(&self, field_name: &str) -> Result<(), String> {
        require_non_empty(&self.id, &format!("{field_name}.id"))?;
        require_non_empty(&self.version, &format!("{field_name}.version"))
    }

    pub fn to_dict(&self) -> Value {
        serde_json::json!({"id": self.id, "version": self.version})
    }

    pub fn from_dict(payload: &Value, field_name: &str) -> Result<Self, String> {
        let reference: Self = serde_json::from_value(payload.clone())
            .map_err(|_| format!("{field_name} must be an object"))?;
        reference.validate(field_name)?;
        Ok(reference)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsetRef {
    pub id: String,
    pub version: String,
    pub schema_digest: String,
}

impl Default for ToolsetRef {
    fn default() -> Self {
        Self {
            id: DEFAULT_TOOLSET_ID.to_string(),
            version: DEFAULT_TOOLSET_VERSION.to_string(),
            schema_digest: DEFAULT_TOOLSET_SCHEMA_DIGEST.to_string(),
        }
    }
}

impl ToolsetRef {
    pub fn validate(&self) -> Result<(), String> {
        CapabilityRef {
            id: self.id.clone(),
            version: self.version.clone(),
        }
        .validate("toolset_ref")?;
        if self.schema_digest.len() != 64
            || self
                .schema_digest
                .bytes()
                .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
        {
            return Err(
                "toolset_ref.schema_digest must be a lowercase SHA-256 hex digest".to_string(),
            );
        }
        Ok(())
    }

    pub fn capability_ref(&self) -> CapabilityRef {
        CapabilityRef {
            id: self.id.clone(),
            version: self.version.clone(),
        }
    }

    pub fn to_dict(&self) -> Value {
        serde_json::json!({
            "id": self.id,
            "version": self.version,
            "schema_digest": self.schema_digest,
        })
    }

    pub fn from_dict(payload: &Value) -> Result<Self, String> {
        let reference: Self = serde_json::from_value(payload.clone())
            .map_err(|_| "toolset_ref must be an object".to_string())?;
        reference.validate()?;
        Ok(reference)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistributedToolPolicy {
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub disallowed_tools: Vec<String>,
    #[serde(default = "default_approval_policy")]
    pub approval: String,
    #[serde(default)]
    pub predicate_ref: Option<CapabilityRef>,
}

impl Default for DistributedToolPolicy {
    fn default() -> Self {
        Self {
            allowed_tools: None,
            disallowed_tools: Vec::new(),
            approval: default_approval_policy(),
            predicate_ref: None,
        }
    }
}

impl DistributedToolPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(
            self.approval.as_str(),
            "default" | "always" | "never" | "on_request"
        ) {
            return Err("tool_policy.approval is unsupported".to_string());
        }
        for (field_name, values) in [
            ("tool_policy.allowed_tools", self.allowed_tools.as_deref()),
            (
                "tool_policy.disallowed_tools",
                Some(self.disallowed_tools.as_slice()),
            ),
        ] {
            if values.is_some_and(|values| values.iter().any(|value| value.trim().is_empty())) {
                return Err(format!("{field_name} must contain non-empty strings"));
            }
        }
        if let Some(reference) = &self.predicate_ref {
            reference.validate("tool_policy.predicate_ref")?;
        }
        Ok(())
    }

    pub fn to_dict(&self) -> Value {
        serde_json::json!({
            "allowed_tools": self.allowed_tools,
            "disallowed_tools": self.disallowed_tools,
            "approval": self.approval,
            "predicate_ref": self.predicate_ref,
        })
    }

    pub fn from_dict(payload: &Value) -> Result<Self, String> {
        let policy: Self = serde_json::from_value(payload.clone())
            .map_err(|_| "tool_policy must be an object".to_string())?;
        policy.validate()?;
        Ok(policy)
    }
}

fn default_approval_policy() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DistributedCapabilities {
    pub toolset_ref: ToolsetRef,
    pub tool_policy: DistributedToolPolicy,
    pub llm_client_ref: Option<CapabilityRef>,
    pub workspace_backend_ref: Option<CapabilityRef>,
    pub approval_provider_ref: Option<CapabilityRef>,
    pub approval_broker_ref: Option<CapabilityRef>,
    pub approval_timeout_seconds: Option<f64>,
    pub cancellation_ref: Option<CapabilityRef>,
    pub event_sink_ref: Option<CapabilityRef>,
    pub app_state_ref: Option<CapabilityRef>,
    pub sub_task_manager_ref: Option<CapabilityRef>,
    #[serde(default)]
    pub memory_provider_refs: Vec<CapabilityRef>,
    #[serde(default)]
    pub hook_refs: Vec<CapabilityRef>,
    #[serde(default)]
    pub observer_refs: Vec<CapabilityRef>,
}

impl DistributedCapabilities {
    pub fn validate(&self) -> Result<(), String> {
        self.toolset_ref.validate()?;
        self.tool_policy.validate()?;
        if self.approval_provider_ref.is_some() != self.approval_broker_ref.is_some() {
            return Err(
                "approval_provider_ref and approval_broker_ref must be declared together"
                    .to_string(),
            );
        }
        if self
            .approval_timeout_seconds
            .is_some_and(|value| !value.is_finite() || value <= 0.0)
        {
            return Err(
                "approval_timeout_seconds must be a finite positive number or null".to_string(),
            );
        }
        for (field_name, reference) in [
            ("llm_client_ref", self.llm_client_ref.as_ref()),
            ("workspace_backend_ref", self.workspace_backend_ref.as_ref()),
            ("approval_provider_ref", self.approval_provider_ref.as_ref()),
            ("approval_broker_ref", self.approval_broker_ref.as_ref()),
            ("cancellation_ref", self.cancellation_ref.as_ref()),
            ("event_sink_ref", self.event_sink_ref.as_ref()),
            ("app_state_ref", self.app_state_ref.as_ref()),
            ("sub_task_manager_ref", self.sub_task_manager_ref.as_ref()),
        ] {
            if let Some(reference) = reference {
                reference.validate(field_name)?;
            }
        }
        for (field_name, references) in [
            ("memory_provider_refs", self.memory_provider_refs.as_slice()),
            ("hook_refs", self.hook_refs.as_slice()),
            ("observer_refs", self.observer_refs.as_slice()),
        ] {
            for (index, reference) in references.iter().enumerate() {
                reference.validate(&format!("capabilities.{field_name}[{index}]"))?;
            }
        }
        Ok(())
    }

    pub fn to_dict(&self) -> Value {
        serde_json::json!({
            "toolset_ref": self.toolset_ref,
            "tool_policy": self.tool_policy,
            "llm_client_ref": self.llm_client_ref,
            "workspace_backend_ref": self.workspace_backend_ref,
            "approval_provider_ref": self.approval_provider_ref,
            "approval_broker_ref": self.approval_broker_ref,
            "approval_timeout_seconds": self.approval_timeout_seconds,
            "cancellation_ref": self.cancellation_ref,
            "event_sink_ref": self.event_sink_ref,
            "app_state_ref": self.app_state_ref,
            "sub_task_manager_ref": self.sub_task_manager_ref,
            "memory_provider_refs": self.memory_provider_refs,
            "hook_refs": self.hook_refs,
            "observer_refs": self.observer_refs,
        })
    }

    pub fn from_dict(payload: &Value) -> Result<Self, String> {
        let capabilities: Self = serde_json::from_value(payload.clone())
            .map_err(|_| "capabilities must be an object".to_string())?;
        capabilities.validate()?;
        Ok(capabilities)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DistributedRunEnvelope {
    pub schema_version: String,
    pub job_id: String,
    pub run_id: String,
    pub task: AgentTask,
    pub recipe: RuntimeRecipe,
    pub cycle_name: String,
    pub cycle_index: u32,
    pub idempotency_key: String,
    pub deadline_unix_ms: Option<u64>,
    pub lease_duration_ms: u64,
}

impl DistributedRunEnvelope {
    #[allow(clippy::too_many_arguments)]
    pub fn for_cycle(
        task: AgentTask,
        recipe: RuntimeRecipe,
        cycle_index: u32,
        cycle_name: impl Into<String>,
        run_id: Option<String>,
        deadline_unix_ms: Option<u64>,
        lease_duration_ms: u64,
    ) -> Result<Self, String> {
        let run_id = run_id
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                task.metadata
                    .get("_vv_agent_run_id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| task.task_id.clone());
        let idempotency_key = format!("{run_id}:cycle:{cycle_index}");
        let envelope = Self {
            schema_version: DISTRIBUTED_RUN_SCHEMA_VERSION.to_string(),
            job_id: idempotency_key.clone(),
            run_id,
            task,
            recipe,
            cycle_name: cycle_name.into(),
            cycle_index,
            idempotency_key,
            deadline_unix_ms,
            lease_duration_ms,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != DISTRIBUTED_RUN_SCHEMA_VERSION {
            return Err(format!(
                "unsupported distributed schema_version: {}",
                self.schema_version
            ));
        }
        for (field_name, value) in [
            ("job_id", self.job_id.as_str()),
            ("run_id", self.run_id.as_str()),
            ("cycle_name", self.cycle_name.as_str()),
            ("idempotency_key", self.idempotency_key.as_str()),
        ] {
            require_non_empty(value, &format!("distributed envelope {field_name}"))?;
        }
        if self.cycle_index == 0 {
            return Err(
                "distributed envelope cycle_index must be between 1 and 4294967295".to_string(),
            );
        }
        if self.lease_duration_ms == 0 {
            return Err(
                "distributed envelope lease_duration_ms must be a positive integer".to_string(),
            );
        }
        self.recipe.validate()?;
        Ok(())
    }

    pub fn ensure_not_expired_at(&self, now_ms: u64) -> Result<(), String> {
        if self
            .deadline_unix_ms
            .is_some_and(|deadline| deadline <= now_ms)
        {
            return Err(format!(
                "distributed job {} deadline has expired",
                self.job_id
            ));
        }
        Ok(())
    }

    pub fn ensure_not_expired(&self) -> Result<(), String> {
        self.ensure_not_expired_at(now_unix_ms()?)
    }

    pub fn remaining_millis_at(&self, now_ms: u64) -> Option<u64> {
        self.deadline_unix_ms
            .map(|deadline| deadline.saturating_sub(now_ms))
    }

    pub fn to_dict(&self) -> Value {
        serde_json::json!({
            "schema_version": self.schema_version,
            "job_id": self.job_id,
            "run_id": self.run_id,
            "task": self.task.to_dict(),
            "recipe": self.recipe.to_dict(),
            "cycle_name": self.cycle_name,
            "cycle_index": self.cycle_index,
            "idempotency_key": self.idempotency_key,
            "deadline_unix_ms": self.deadline_unix_ms,
            "lease_duration_ms": self.lease_duration_ms,
        })
    }

    pub fn from_dict(payload: &Value) -> Result<Self, String> {
        let envelope: Self = serde_json::from_value(payload.clone())
            .map_err(|_| "distributed envelope must be an object".to_string())?;
        envelope.validate()?;
        Ok(envelope)
    }
}

pub fn now_unix_ms() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_millis()
        .try_into()
        .map_err(|_| "system clock milliseconds exceed u64".to_string())
}

fn require_non_empty(value: &str, field_name: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{field_name} must be a non-empty string"))
    } else {
        Ok(())
    }
}
