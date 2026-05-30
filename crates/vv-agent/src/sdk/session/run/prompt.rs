use std::collections::BTreeMap;

use serde_json::Value;

use crate::sdk::types::AgentRun;
use crate::types::AgentStatus;

use super::super::util::normalize_session_prompt;
use super::super::AgentSession;

impl AgentSession {
    pub fn prompt(&mut self, prompt: impl Into<String>) -> Result<AgentRun, String> {
        self.prompt_with_auto_follow_up(prompt, true)
    }

    pub fn prompt_with_auto_follow_up(
        &mut self,
        prompt: impl Into<String>,
        auto_follow_up: bool,
    ) -> Result<AgentRun, String> {
        let mut run = self.run_once(normalize_session_prompt(prompt.into(), "prompt")?)?;
        if !auto_follow_up {
            return Ok(run);
        }

        while run.result.status == AgentStatus::Completed {
            let follow_up_prompt = self
                .follow_up_queue
                .lock()
                .expect("session follow-up queue lock")
                .pop_front();
            let Some(follow_up_prompt) = follow_up_prompt else {
                break;
            };
            self.emit(
                "session_follow_up_dequeued",
                BTreeMap::from([(
                    "prompt".to_string(),
                    Value::String(follow_up_prompt.clone()),
                )]),
            );
            run = self.run_once(follow_up_prompt)?;
        }
        Ok(run)
    }
}
