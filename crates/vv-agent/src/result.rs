use std::any::type_name;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use serde::de::DeserializeOwned;
use serde::{Serialize, Serializer};
use serde_json::{json, Value};
use thiserror::Error;

use crate::agent::Agent;
use crate::budget::{BudgetExhaustion, BudgetUsageSnapshot};
use crate::checkpoint::ResumeObservation;
use crate::config::ResolvedModelConfig;
use crate::events::RunEvent;
use crate::run_config::RunConfig;
use crate::tools::{ToolContext, ToolOrchestrator, ToolRunOptions};
use crate::types::{AgentResult, AgentStatus, CompletionReason, Message, Metadata, TaskTokenUsage};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ApprovalSnapshot {
    pub interruption_id: String,
    pub tool_name: String,
    pub tool_call_id: String,
    pub arguments: BTreeMap<String, Value>,
    pub message: String,
    pub cycle_index: Option<u32>,
    pub approved: bool,
}

#[derive(Debug, Error)]
pub enum FinalOutputError {
    #[error("run result for agent `{agent_name}` has no final output (status: {status:?})")]
    Missing {
        agent_name: String,
        status: AgentStatus,
    },
    #[error("failed to deserialize final output for agent `{agent_name}` as `{target}`: {source}")]
    Deserialize {
        agent_name: String,
        target: &'static str,
        #[source]
        source: serde_json::Error,
    },
}

#[derive(Clone)]
pub struct RunResult {
    agent_name: String,
    run_id: String,
    trace_id: String,
    input: String,
    new_items: Vec<Message>,
    events: Vec<RunEvent>,
    token_usage: TaskTokenUsage,
    metadata: Metadata,
    result: AgentResult,
    resolved: Option<ResolvedModelConfig>,
    resume_context: Option<RunResumeContext>,
}

impl RunResult {
    pub fn new(
        agent_name: impl Into<String>,
        result: AgentResult,
        resolved: ResolvedModelConfig,
    ) -> Self {
        let token_usage = result.token_usage.clone();
        Self {
            agent_name: agent_name.into(),
            run_id: String::new(),
            trace_id: String::new(),
            input: String::new(),
            new_items: Vec::new(),
            events: Vec::new(),
            token_usage,
            metadata: Metadata::new(),
            result,
            resolved: Some(resolved),
            resume_context: None,
        }
    }

    pub(crate) fn without_resolved_model(
        agent_name: impl Into<String>,
        result: AgentResult,
    ) -> Self {
        let token_usage = result.token_usage.clone();
        Self {
            agent_name: agent_name.into(),
            run_id: String::new(),
            trace_id: String::new(),
            input: String::new(),
            new_items: Vec::new(),
            events: Vec::new(),
            token_usage,
            metadata: Metadata::new(),
            result,
            resolved: None,
            resume_context: None,
        }
    }

    pub fn agent_name(&self) -> &str {
        &self.agent_name
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn new_items(&self) -> &[Message] {
        &self.new_items
    }

    pub fn events(&self) -> &[RunEvent] {
        &self.events
    }

    pub fn token_usage(&self) -> &TaskTokenUsage {
        &self.token_usage
    }

    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    pub fn status(&self) -> AgentStatus {
        self.result.status
    }

    pub fn completion_reason(&self) -> Option<CompletionReason> {
        self.result.completion_reason
    }

    pub fn completion_tool_name(&self) -> Option<&str> {
        self.result.completion_tool_name.as_deref()
    }

    pub fn partial_output(&self) -> Option<&str> {
        self.result.partial_output.as_deref()
    }

    pub fn budget_usage(&self) -> Option<&BudgetUsageSnapshot> {
        self.result.budget_usage.as_ref()
    }

    pub fn budget_exhaustion(&self) -> Option<&BudgetExhaustion> {
        self.result.budget_exhaustion.as_ref()
    }

    pub fn checkpoint_key(&self) -> Option<&str> {
        self.result.checkpoint_key.as_deref()
    }

    pub fn resume_observation(&self) -> Option<&ResumeObservation> {
        self.result.resume_observation.as_ref()
    }

    pub fn final_output(&self) -> Option<&str> {
        self.result
            .final_answer
            .as_deref()
            .or(self.result.wait_reason.as_deref())
            .or(self.result.error.as_deref())
    }

    pub fn error_code(&self) -> Option<&str> {
        self.result.error_code.as_deref()
    }

    pub fn deserialize<T>(&self) -> Result<T, FinalOutputError>
    where
        T: DeserializeOwned,
    {
        let output = self
            .final_output()
            .ok_or_else(|| FinalOutputError::Missing {
                agent_name: self.agent_name.clone(),
                status: self.status(),
            })?;
        serde_json::from_str(output).map_err(|source| FinalOutputError::Deserialize {
            agent_name: self.agent_name.clone(),
            target: type_name::<T>(),
            source,
        })
    }

    pub fn result(&self) -> &AgentResult {
        &self.result
    }

    pub fn approvals(&self) -> Vec<ApprovalSnapshot> {
        approval_snapshots(&self.result, &BTreeSet::new())
    }

    pub fn approval_snapshot(&self) -> Vec<ApprovalSnapshot> {
        self.approvals()
    }

    pub fn resolved_model(&self) -> Option<&ResolvedModelConfig> {
        self.resolved.as_ref()
    }

    pub fn to_value(&self) -> Value {
        let result = self.result.to_dict();
        let status = result.get("status").cloned().unwrap_or(Value::Null);
        let token_usage = result
            .get("token_usage")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let mut payload = json!({
            "input": self.input,
            "new_items": self.new_items.iter().map(Message::to_dict).collect::<Vec<_>>(),
            "final_output": self.final_output(),
            "status": status,
            "events": self.events,
            "token_usage": token_usage,
            "trace_id": self.trace_id,
            "run_id": self.run_id,
            "metadata": self.metadata,
            "agent_name": self.agent_name,
            "completion_reason": self.result.completion_reason,
            "completion_tool_name": self.result.completion_tool_name,
            "partial_output": self.result.partial_output,
            "budget_usage": self.result.budget_usage,
            "budget_exhaustion": self.result.budget_exhaustion,
            "checkpoint_key": self.result.checkpoint_key,
            "resume_observation": self.result.resume_observation,
            "resolved_model": self.resolved.as_ref().map(resolved_model_public_value),
        });
        if let Some(error_code) = self.error_code() {
            payload["error_code"] = Value::String(error_code.to_string());
        }
        payload
    }

    pub fn to_dict(&self) -> Value {
        self.to_value()
    }

    pub fn with_input(mut self, input: impl Into<String>) -> Self {
        self.input = input.into();
        self
    }

    pub fn with_new_items(mut self, new_items: Vec<Message>) -> Self {
        self.new_items = new_items;
        self
    }

    pub fn with_events(mut self, events: Vec<RunEvent>) -> Self {
        self.events = events;
        self
    }

    pub fn with_token_usage(mut self, token_usage: TaskTokenUsage) -> Self {
        self.token_usage = token_usage;
        self
    }

    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    pub fn into_state(self) -> Result<RunState, String> {
        RunState::from_result(self)
    }

    pub(crate) fn with_resume_context(mut self, context: RunResumeContext) -> Self {
        self.resume_context = Some(context);
        self
    }

    pub(crate) fn with_ids(
        mut self,
        run_id: impl Into<String>,
        trace_id: impl Into<String>,
    ) -> Self {
        self.run_id = run_id.into();
        self.trace_id = trace_id.into();
        self
    }

    pub(crate) fn resume_context(&self) -> Option<&RunResumeContext> {
        self.resume_context.as_ref()
    }
}

impl Serialize for RunResult {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.to_value().serialize(serializer)
    }
}

#[derive(Clone)]
pub struct RunState {
    result: RunResult,
    approved_interruption_ids: Vec<String>,
    approval_consumption: Arc<Mutex<BTreeSet<String>>>,
}

impl RunState {
    pub fn from_result(result: RunResult) -> Result<Self, String> {
        if result.status() != AgentStatus::WaitUser {
            return Err("only interrupted runs can be converted into RunState".to_string());
        }
        Ok(Self {
            result,
            approved_interruption_ids: Vec::new(),
            approval_consumption: Arc::new(Mutex::new(BTreeSet::new())),
        })
    }

    pub fn result(&self) -> &RunResult {
        &self.result
    }

    pub fn approve(&mut self, interruption_id: &str) -> Result<(), String> {
        if !self
            .pending_approval_ids()
            .iter()
            .any(|id| id == interruption_id)
        {
            return Err(format!("unknown approval interruption: {interruption_id}"));
        }
        if !self
            .approved_interruption_ids
            .iter()
            .any(|id| id == interruption_id)
        {
            self.approved_interruption_ids
                .push(interruption_id.to_string());
        }
        Ok(())
    }

    pub fn approved_interruption_ids(&self) -> &[String] {
        &self.approved_interruption_ids
    }

    pub fn approvals(&self) -> Vec<ApprovalSnapshot> {
        let approved = self
            .approved_interruption_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        approval_snapshots(self.result.result(), &approved)
    }

    pub fn approval_snapshot(&self) -> Vec<ApprovalSnapshot> {
        self.approvals()
    }

    pub fn pending_approval_ids(&self) -> Vec<String> {
        self.result
            .approvals()
            .into_iter()
            .map(|snapshot| snapshot.interruption_id)
            .collect()
    }

    pub(crate) fn into_inner(self) -> (RunResult, Vec<String>, Arc<Mutex<BTreeSet<String>>>) {
        (
            self.result,
            self.approved_interruption_ids,
            self.approval_consumption,
        )
    }
}

fn approval_snapshots(
    result: &AgentResult,
    approved_interruption_ids: &BTreeSet<String>,
) -> Vec<ApprovalSnapshot> {
    let mut seen = BTreeSet::new();
    let mut snapshots = Vec::new();
    for cycle in &result.cycles {
        for tool_result in &cycle.tool_results {
            let metadata = &tool_result.metadata;
            let interruption_id = metadata
                .get("approval_interruption_id")
                .or_else(|| metadata.get("request_id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let Some(interruption_id) = interruption_id else {
                continue;
            };
            let approval_requested = metadata
                .get("mode")
                .and_then(Value::as_str)
                .is_some_and(|mode| mode == "approval_requested")
                || metadata
                    .get("approval_required")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            if !approval_requested || !seen.insert(interruption_id.to_string()) {
                continue;
            }
            let arguments = metadata
                .get("arguments")
                .and_then(Value::as_object)
                .map(|arguments| {
                    arguments
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect()
                })
                .unwrap_or_default();
            snapshots.push(ApprovalSnapshot {
                interruption_id: interruption_id.to_string(),
                tool_name: metadata
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                tool_call_id: tool_result.tool_call_id.clone(),
                arguments,
                message: metadata
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or(&tool_result.content)
                    .to_string(),
                cycle_index: Some(cycle.index),
                approved: approved_interruption_ids.contains(interruption_id),
            });
        }
    }
    snapshots
}

fn resolved_model_public_value(resolved: &ResolvedModelConfig) -> Value {
    json!({
        "backend": resolved.backend,
        "requested_model": resolved.requested_model,
        "selected_model": resolved.selected_model,
        "model_id": resolved.model_id,
        "endpoint": resolved
            .endpoint_options
            .first()
            .map(|option| option.endpoint.endpoint_id.as_str()),
    })
}

#[derive(Clone)]
pub(crate) struct RunResumeContext {
    pub agent: Agent,
    pub input: crate::runner::NormalizedInput,
    pub config: RunConfig,
    pub runner: crate::runner::Runner,
    pub pending_tool_approval: Option<PendingToolApproval>,
}

#[derive(Clone)]
pub(crate) struct PendingToolApproval {
    pub interruption_id: String,
    pub call: crate::types::ToolCall,
    pub cycle_index: u32,
    pub context: ToolContext,
    pub options: ToolRunOptions,
    pub orchestrator: ToolOrchestrator,
    pub task: crate::types::AgentTask,
    pub hook_manager: crate::runtime::RuntimeHookManager,
}
