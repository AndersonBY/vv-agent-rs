use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::budget::{RunBudgetLimits, MAX_WIRE_INTEGER};
use crate::checkpoint::{
    validate_checkpoint_key, validate_extension_namespace, validate_sha256, AmbiguousModelPolicy,
    AmbiguousToolPolicy, ClaimMode, ResumePolicy, DEFAULT_MAX_EXTENSION_STATE_BYTES,
    RUN_DEFINITION_SCHEMA,
};
use crate::types::AgentTask;

use super::super::RuntimeRecipe;

pub const DISTRIBUTED_RUN_SCHEMA_VERSION_V1: &str = "vv-agent.distributed-run.v1";
pub const DISTRIBUTED_RUN_SCHEMA_VERSION_V2: &str = "vv-agent.distributed-run.v2";
/// Backwards-compatible alias used by existing v1 callers.
pub const DISTRIBUTED_RUN_SCHEMA_VERSION: &str = DISTRIBUTED_RUN_SCHEMA_VERSION_V1;
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistributedCheckpointExtensionRef {
    pub namespace: String,
    pub reference: CapabilityRef,
    #[serde(default)]
    pub required: bool,
}

impl DistributedCheckpointExtensionRef {
    pub fn validate(&self) -> Result<(), String> {
        validate_extension_namespace(&self.namespace).map_err(|error| error.to_string())?;
        self.reference
            .validate("checkpoint_extension_ref.reference")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistributedCheckpointConfig {
    pub key: String,
    #[serde(default)]
    pub resume_policy: ResumePolicy,
    #[serde(default)]
    pub ambiguous_model_policy: AmbiguousModelPolicy,
    #[serde(default)]
    pub ambiguous_tool_policy: AmbiguousToolPolicy,
    #[serde(default)]
    pub required_extension_namespaces: Vec<String>,
    #[serde(default = "default_max_extension_state_bytes")]
    pub max_extension_state_bytes: u64,
    #[serde(default)]
    pub credential_slots: Vec<String>,
}

impl DistributedCheckpointConfig {
    pub fn validate(&self) -> Result<(), String> {
        validate_checkpoint_key(&self.key).map_err(|error| error.to_string())?;
        if self.max_extension_state_bytes > MAX_WIRE_INTEGER {
            return Err(format!(
                "max_extension_state_bytes must be between 0 and {MAX_WIRE_INTEGER}"
            ));
        }
        validate_sorted_unique(
            &self.required_extension_namespaces,
            "required_extension_namespaces",
            |namespace| validate_extension_namespace(namespace).map_err(|error| error.to_string()),
        )?;
        for pointer in &self.credential_slots {
            validate_json_pointer(pointer)?;
        }
        if self
            .credential_slots
            .windows(2)
            .any(|window| utf16_cmp(&window[0], &window[1]) != std::cmp::Ordering::Less)
        {
            return Err("credential_slots must be sorted and unique".to_string());
        }
        Ok(())
    }

    pub fn to_dict(&self) -> Value {
        serde_json::to_value(self).expect("validated checkpoint config always serializes")
    }
}

fn default_max_extension_state_bytes() -> u64 {
    DEFAULT_MAX_EXTENSION_STATE_BYTES
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
    pub host_cost_meter_ref: Option<CapabilityRef>,
    pub app_state_ref: Option<CapabilityRef>,
    pub sub_task_manager_ref: Option<CapabilityRef>,
    #[serde(default)]
    pub memory_provider_refs: Vec<CapabilityRef>,
    #[serde(default)]
    pub hook_refs: Vec<CapabilityRef>,
    #[serde(default)]
    pub observer_refs: Vec<CapabilityRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_store_ref: Option<CapabilityRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_event_store_ref: Option<CapabilityRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checkpoint_extension_refs: Vec<DistributedCheckpointExtensionRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconciliation_provider_ref: Option<CapabilityRef>,
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
            ("host_cost_meter_ref", self.host_cost_meter_ref.as_ref()),
            ("app_state_ref", self.app_state_ref.as_ref()),
            ("sub_task_manager_ref", self.sub_task_manager_ref.as_ref()),
            ("checkpoint_store_ref", self.checkpoint_store_ref.as_ref()),
            (
                "checkpoint_event_store_ref",
                self.checkpoint_event_store_ref.as_ref(),
            ),
            (
                "reconciliation_provider_ref",
                self.reconciliation_provider_ref.as_ref(),
            ),
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
        let mut namespaces = BTreeSet::new();
        for reference in &self.checkpoint_extension_refs {
            reference.validate()?;
            if !namespaces.insert(reference.namespace.as_str()) {
                return Err(format!(
                    "duplicate checkpoint extension namespace {}",
                    reference.namespace
                ));
            }
        }
        Ok(())
    }

    pub fn to_dict(&self) -> Value {
        let mut value = serde_json::json!({
            "toolset_ref": self.toolset_ref,
            "tool_policy": self.tool_policy,
            "llm_client_ref": self.llm_client_ref,
            "workspace_backend_ref": self.workspace_backend_ref,
            "approval_provider_ref": self.approval_provider_ref,
            "approval_broker_ref": self.approval_broker_ref,
            "approval_timeout_seconds": self.approval_timeout_seconds,
            "cancellation_ref": self.cancellation_ref,
            "event_sink_ref": self.event_sink_ref,
            "host_cost_meter_ref": self.host_cost_meter_ref,
            "app_state_ref": self.app_state_ref,
            "sub_task_manager_ref": self.sub_task_manager_ref,
            "memory_provider_refs": self.memory_provider_refs,
            "hook_refs": self.hook_refs,
            "observer_refs": self.observer_refs,
        });
        let object = value
            .as_object_mut()
            .expect("distributed capabilities are always an object");
        for (name, reference) in [
            ("checkpoint_store_ref", self.checkpoint_store_ref.as_ref()),
            (
                "checkpoint_event_store_ref",
                self.checkpoint_event_store_ref.as_ref(),
            ),
            (
                "reconciliation_provider_ref",
                self.reconciliation_provider_ref.as_ref(),
            ),
        ] {
            if let Some(reference) = reference {
                object.insert(name.to_string(), reference.to_dict());
            }
        }
        if !self.checkpoint_extension_refs.is_empty() {
            object.insert(
                "checkpoint_extension_refs".to_string(),
                serde_json::to_value(&self.checkpoint_extension_refs)
                    .expect("validated checkpoint extension references always serialize"),
            );
        }
        value
    }

    pub fn from_dict(payload: &Value) -> Result<Self, String> {
        let capabilities: Self = serde_json::from_value(payload.clone())
            .map_err(|_| "capabilities must be an object".to_string())?;
        capabilities.validate()?;
        Ok(capabilities)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DistributedRunEnvelope {
    pub schema_version: String,
    pub job_id: String,
    pub run_id: String,
    pub task: AgentTask,
    #[serde(default)]
    pub budget_limits: Option<RunBudgetLimits>,
    pub recipe: RuntimeRecipe,
    pub cycle_name: String,
    pub cycle_index: u32,
    pub idempotency_key: String,
    pub deadline_unix_ms: Option<u64>,
    pub lease_duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_definition_schema: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_definition_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_mode: Option<ClaimMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_attempt: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_config: Option<DistributedCheckpointConfig>,
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
        budget_limits: Option<RunBudgetLimits>,
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
            budget_limits,
            recipe,
            cycle_name: cycle_name.into(),
            cycle_index,
            idempotency_key,
            deadline_unix_ms,
            lease_duration_ms,
            root_run_id: None,
            trace_id: None,
            run_definition_schema: None,
            run_definition_digest: None,
            claim_mode: None,
            resume_attempt: None,
            checkpoint_config: None,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn enable_checkpoint_v2(
        mut self,
        root_run_id: impl Into<String>,
        trace_id: impl Into<String>,
        run_definition_digest: impl Into<String>,
        claim_mode: ClaimMode,
        resume_attempt: u64,
        checkpoint_config: DistributedCheckpointConfig,
    ) -> Result<Self, String> {
        self.schema_version = DISTRIBUTED_RUN_SCHEMA_VERSION_V2.to_string();
        self.root_run_id = Some(root_run_id.into());
        self.trace_id = Some(trace_id.into());
        self.run_definition_schema = Some(RUN_DEFINITION_SCHEMA.to_string());
        self.run_definition_digest = Some(run_definition_digest.into());
        self.claim_mode = Some(claim_mode);
        self.resume_attempt = Some(resume_attempt);
        self.checkpoint_config = Some(checkpoint_config);
        self.validate()?;
        Ok(self)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn for_checkpoint_cycle(
        task: AgentTask,
        recipe: RuntimeRecipe,
        cycle_index: u32,
        cycle_name: impl Into<String>,
        run_id: Option<String>,
        deadline_unix_ms: Option<u64>,
        lease_duration_ms: u64,
        budget_limits: Option<RunBudgetLimits>,
        root_run_id: impl Into<String>,
        trace_id: impl Into<String>,
        run_definition_digest: impl Into<String>,
        claim_mode: ClaimMode,
        resume_attempt: u64,
        checkpoint_config: DistributedCheckpointConfig,
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
            schema_version: DISTRIBUTED_RUN_SCHEMA_VERSION_V2.to_string(),
            job_id: idempotency_key.clone(),
            run_id,
            task,
            budget_limits,
            recipe,
            cycle_name: cycle_name.into(),
            cycle_index,
            idempotency_key,
            deadline_unix_ms,
            lease_duration_ms,
            root_run_id: Some(root_run_id.into()),
            trace_id: Some(trace_id.into()),
            run_definition_schema: Some(RUN_DEFINITION_SCHEMA.to_string()),
            run_definition_digest: Some(run_definition_digest.into()),
            claim_mode: Some(claim_mode),
            resume_attempt: Some(resume_attempt),
            checkpoint_config: Some(checkpoint_config),
        };
        envelope.validate()?;
        Ok(envelope)
    }

    pub fn is_checkpoint_v2(&self) -> bool {
        self.schema_version == DISTRIBUTED_RUN_SCHEMA_VERSION_V2
    }

    pub fn validate(&self) -> Result<(), String> {
        match self.schema_version.as_str() {
            DISTRIBUTED_RUN_SCHEMA_VERSION_V1 => self.validate_v1_fields()?,
            DISTRIBUTED_RUN_SCHEMA_VERSION_V2 => self.validate_v2_fields()?,
            _ => {
                return Err(format!(
                    "unsupported distributed schema_version: {}",
                    self.schema_version
                ));
            }
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
        if let Some(limits) = &self.budget_limits {
            limits.validate()?;
        }
        Ok(())
    }

    fn validate_v1_fields(&self) -> Result<(), String> {
        if self.root_run_id.is_some()
            || self.trace_id.is_some()
            || self.run_definition_schema.is_some()
            || self.run_definition_digest.is_some()
            || self.claim_mode.is_some()
            || self.resume_attempt.is_some()
            || self.checkpoint_config.is_some()
            || self.recipe.capabilities.checkpoint_store_ref.is_some()
            || self
                .recipe
                .capabilities
                .checkpoint_event_store_ref
                .is_some()
            || !self
                .recipe
                .capabilities
                .checkpoint_extension_refs
                .is_empty()
            || self
                .recipe
                .capabilities
                .reconciliation_provider_ref
                .is_some()
        {
            return Err("distributed v1 envelope cannot carry checkpoint v2 fields".to_string());
        }
        Ok(())
    }

    fn validate_v2_fields(&self) -> Result<(), String> {
        for (field_name, value) in [
            ("root_run_id", self.root_run_id.as_deref()),
            ("trace_id", self.trace_id.as_deref()),
        ] {
            let value = value.ok_or_else(|| format!("distributed v2 requires {field_name}"))?;
            require_non_empty(value, &format!("distributed envelope {field_name}"))?;
        }
        if self.run_definition_schema.as_deref() != Some(RUN_DEFINITION_SCHEMA) {
            return Err("checkpoint_definition_schema_unsupported".to_string());
        }
        let digest = self
            .run_definition_digest
            .as_deref()
            .ok_or_else(|| "distributed v2 requires run_definition_digest".to_string())?;
        validate_sha256(digest, "run_definition_digest").map_err(|error| error.to_string())?;
        if self.claim_mode.is_none() {
            return Err("checkpoint_claim_mode_invalid".to_string());
        }
        let resume_attempt = self
            .resume_attempt
            .ok_or_else(|| "distributed v2 requires resume_attempt".to_string())?;
        if resume_attempt == 0 || resume_attempt > MAX_WIRE_INTEGER {
            return Err("checkpoint_resume_attempt_invalid".to_string());
        }
        let config = self
            .checkpoint_config
            .as_ref()
            .ok_or_else(|| "distributed v2 requires checkpoint_config".to_string())?;
        config.validate()?;
        if self.recipe.capabilities.checkpoint_store_ref.is_none() {
            return Err("distributed v2 requires checkpoint_store_ref".to_string());
        }
        for namespace in &config.required_extension_namespaces {
            if !self
                .recipe
                .capabilities
                .checkpoint_extension_refs
                .iter()
                .any(|reference| reference.namespace == *namespace && reference.required)
            {
                return Err(format!(
                    "required checkpoint extension {namespace} is unavailable"
                ));
            }
        }
        if self
            .deadline_unix_ms
            .is_some_and(|value| value > MAX_WIRE_INTEGER)
            || self.lease_duration_ms > MAX_WIRE_INTEGER
        {
            return Err("distributed v2 lease values must be JSON-safe integers".to_string());
        }
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
        let mut value = serde_json::json!({
            "schema_version": self.schema_version,
            "job_id": self.job_id,
            "run_id": self.run_id,
            "task": self.task.to_dict(),
            "budget_limits": self.budget_limits,
            "recipe": self.recipe.to_dict(),
            "cycle_name": self.cycle_name,
            "cycle_index": self.cycle_index,
            "idempotency_key": self.idempotency_key,
            "deadline_unix_ms": self.deadline_unix_ms,
            "lease_duration_ms": self.lease_duration_ms,
        });
        if self.is_checkpoint_v2() {
            let object = value
                .as_object_mut()
                .expect("distributed envelope is always an object");
            object.insert(
                "root_run_id".to_string(),
                serde_json::to_value(&self.root_run_id)
                    .expect("distributed identity always serializes"),
            );
            object.insert(
                "trace_id".to_string(),
                serde_json::to_value(&self.trace_id)
                    .expect("distributed identity always serializes"),
            );
            object.insert(
                "run_definition_schema".to_string(),
                serde_json::to_value(&self.run_definition_schema)
                    .expect("run definition schema always serializes"),
            );
            object.insert(
                "run_definition_digest".to_string(),
                serde_json::to_value(&self.run_definition_digest)
                    .expect("run definition digest always serializes"),
            );
            object.insert(
                "claim_mode".to_string(),
                serde_json::to_value(self.claim_mode).expect("claim mode always serializes"),
            );
            object.insert(
                "resume_attempt".to_string(),
                serde_json::to_value(self.resume_attempt)
                    .expect("resume attempt always serializes"),
            );
            object.insert(
                "checkpoint_config".to_string(),
                serde_json::to_value(&self.checkpoint_config)
                    .expect("checkpoint config always serializes"),
            );
            normalize_integral_float(
                object
                    .get_mut("recipe")
                    .and_then(Value::as_object_mut)
                    .expect("runtime recipe is always an object"),
                "timeout_seconds",
            );
            if let Some(capabilities) = object
                .get_mut("recipe")
                .and_then(Value::as_object_mut)
                .and_then(|recipe| recipe.get_mut("capabilities"))
                .and_then(Value::as_object_mut)
            {
                normalize_integral_float(capabilities, "approval_timeout_seconds");
            }
        }
        value
    }

    pub fn from_dict(payload: &Value) -> Result<Self, String> {
        if !payload.is_object() {
            return Err("distributed envelope must be an object".to_string());
        }
        if let Some(budget_limits) = payload
            .get("budget_limits")
            .filter(|budget_limits| !budget_limits.is_null())
        {
            serde_json::from_value::<RunBudgetLimits>(budget_limits.clone()).map_err(|error| {
                format!(
                    "distributed envelope budget limit must be between 0 and {MAX_WIRE_INTEGER}: {error}"
                )
            })?;
        }
        let schema_version = payload
            .get("schema_version")
            .and_then(Value::as_str)
            .ok_or_else(|| "unsupported distributed schema_version".to_string())?;
        if !matches!(
            schema_version,
            DISTRIBUTED_RUN_SCHEMA_VERSION_V1 | DISTRIBUTED_RUN_SCHEMA_VERSION_V2
        ) {
            return Err(format!(
                "unsupported distributed schema_version: {schema_version}"
            ));
        }
        if schema_version == DISTRIBUTED_RUN_SCHEMA_VERSION_V2 {
            let has_v2_shape = [
                "root_run_id",
                "trace_id",
                "run_definition_schema",
                "run_definition_digest",
                "claim_mode",
                "resume_attempt",
                "checkpoint_config",
            ]
            .iter()
            .any(|field| payload.get(*field).is_some());
            if !has_v2_shape {
                return Err(format!(
                    "unsupported distributed schema_version: {schema_version}"
                ));
            }
            validate_v2_discriminator_fields(payload)?;
        }
        let envelope: Self =
            serde_json::from_value(payload.clone()).map_err(|error| error.to_string())?;
        envelope.validate()?;
        Ok(envelope)
    }
}

impl Serialize for DistributedRunEnvelope {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if self.is_checkpoint_v2() {
            return self.to_dict().serialize(serializer);
        }
        #[derive(Serialize)]
        struct V1Envelope<'a> {
            schema_version: &'a str,
            job_id: &'a str,
            run_id: &'a str,
            task: &'a AgentTask,
            budget_limits: &'a Option<RunBudgetLimits>,
            recipe: &'a RuntimeRecipe,
            cycle_name: &'a str,
            cycle_index: u32,
            idempotency_key: &'a str,
            deadline_unix_ms: Option<u64>,
            lease_duration_ms: u64,
        }
        V1Envelope {
            schema_version: &self.schema_version,
            job_id: &self.job_id,
            run_id: &self.run_id,
            task: &self.task,
            budget_limits: &self.budget_limits,
            recipe: &self.recipe,
            cycle_name: &self.cycle_name,
            cycle_index: self.cycle_index,
            idempotency_key: &self.idempotency_key,
            deadline_unix_ms: self.deadline_unix_ms,
            lease_duration_ms: self.lease_duration_ms,
        }
        .serialize(serializer)
    }
}

fn validate_v2_discriminator_fields(payload: &Value) -> Result<(), String> {
    if payload.get("run_definition_schema").and_then(Value::as_str) != Some(RUN_DEFINITION_SCHEMA) {
        return Err("checkpoint_definition_schema_unsupported".to_string());
    }
    if payload.get("checkpoint_config").is_none_or(Value::is_null) {
        return Err("distributed v2 requires checkpoint_config".to_string());
    }
    if !matches!(
        payload.get("claim_mode").and_then(Value::as_str),
        Some("continue" | "recovery")
    ) {
        return Err("checkpoint_claim_mode_invalid".to_string());
    }
    Ok(())
}

fn normalize_integral_float(object: &mut serde_json::Map<String, Value>, field: &str) {
    let Some(value) = object.get(field).and_then(Value::as_f64) else {
        return;
    };
    if value.is_finite() && value >= 0.0 && value.fract() == 0.0 && value <= MAX_WIRE_INTEGER as f64
    {
        object.insert(field.to_string(), Value::from(value as u64));
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

fn validate_sorted_unique(
    values: &[String],
    field_name: &str,
    validate: impl Fn(&str) -> Result<(), String>,
) -> Result<(), String> {
    for value in values {
        validate(value)?;
    }
    if values.windows(2).any(|window| window[0] >= window[1]) {
        return Err(format!("{field_name} must be sorted and unique"));
    }
    Ok(())
}

fn validate_json_pointer(pointer: &str) -> Result<(), String> {
    if pointer.is_empty() {
        return Ok(());
    }
    if !pointer.starts_with('/') {
        return Err("credential_slots must contain RFC 6901 JSON pointers".to_string());
    }
    let bytes = pointer.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'~' {
            let Some(next) = bytes.get(index + 1) else {
                return Err("credential_slots must contain RFC 6901 JSON pointers".to_string());
            };
            if !matches!(*next, b'0' | b'1') {
                return Err("credential_slots must contain RFC 6901 JSON pointers".to_string());
            }
            index += 1;
        }
        index += 1;
    }
    Ok(())
}

fn utf16_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}
