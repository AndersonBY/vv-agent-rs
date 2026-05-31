use crate::agent::Agent;
use crate::config::ResolvedModelConfig;
use crate::run_config::RunConfig;
use crate::sdk::types::AgentRun;
use crate::types::{AgentResult, AgentStatus};

#[derive(Clone)]
pub struct RunResult {
    agent_name: String,
    result: AgentResult,
    resolved: ResolvedModelConfig,
    resume_context: Option<RunResumeContext>,
}

impl RunResult {
    pub fn new(
        agent_name: impl Into<String>,
        result: AgentResult,
        resolved: ResolvedModelConfig,
    ) -> Self {
        Self {
            agent_name: agent_name.into(),
            result,
            resolved,
            resume_context: None,
        }
    }

    pub fn agent_name(&self) -> &str {
        &self.agent_name
    }

    pub fn status(&self) -> AgentStatus {
        self.result.status
    }

    pub fn final_output(&self) -> Option<&str> {
        self.result.final_answer.as_deref()
    }

    pub fn result(&self) -> &AgentResult {
        &self.result
    }

    pub fn resolved_model(&self) -> &ResolvedModelConfig {
        &self.resolved
    }

    pub fn into_state(self) -> Result<RunState, String> {
        RunState::from_result(self)
    }

    pub(crate) fn with_resume_context(mut self, context: RunResumeContext) -> Self {
        self.resume_context = Some(context);
        self
    }

    pub(crate) fn resume_context(&self) -> Option<&RunResumeContext> {
        self.resume_context.as_ref()
    }
}

impl From<AgentRun> for RunResult {
    fn from(run: AgentRun) -> Self {
        Self::new(run.agent_name, run.result, run.resolved)
    }
}

#[derive(Clone)]
pub struct RunState {
    result: RunResult,
    approved_interruption_ids: Vec<String>,
}

impl RunState {
    pub fn from_result(result: RunResult) -> Result<Self, String> {
        if result.status() != AgentStatus::WaitUser {
            return Err("only interrupted runs can be converted into RunState".to_string());
        }
        Ok(Self {
            result,
            approved_interruption_ids: Vec::new(),
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

    pub fn pending_approval_ids(&self) -> Vec<String> {
        self.result
            .result()
            .cycles
            .iter()
            .flat_map(|cycle| cycle.tool_results.iter())
            .filter_map(|tool_result| {
                tool_result
                    .metadata
                    .get("approval_interruption_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .collect()
    }

    pub(crate) fn into_inner(self) -> (RunResult, Vec<String>) {
        (self.result, self.approved_interruption_ids)
    }
}

#[derive(Clone)]
pub(crate) struct RunResumeContext {
    pub agent: Agent,
    pub input: crate::runner::NormalizedInput,
    pub config: RunConfig,
}
