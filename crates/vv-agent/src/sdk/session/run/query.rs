use crate::sdk::types::agent_status_value;
use crate::types::AgentStatus;

use super::super::AgentSession;

impl AgentSession {
    pub fn query(&mut self, prompt: impl Into<String>) -> Result<String, String> {
        self.query_with_require_completed(prompt, true)
    }

    pub fn query_with_require_completed(
        &mut self,
        prompt: impl Into<String>,
        require_completed: bool,
    ) -> Result<String, String> {
        let run = self.prompt(prompt)?;
        if run.result.status == AgentStatus::Completed {
            return Ok(run.result.final_answer.unwrap_or_default());
        }
        if require_completed {
            let reason = run
                .result
                .error
                .clone()
                .or(run.result.wait_reason.clone())
                .or(run.result.final_answer.clone())
                .unwrap_or_else(|| "session query did not complete".to_string());
            return Err(format!(
                "Session query failed with status={}: {}",
                agent_status_value(run.result.status),
                reason
            ));
        }
        Ok(run
            .result
            .final_answer
            .or(run.result.wait_reason)
            .or(run.result.error)
            .unwrap_or_default())
    }
}
