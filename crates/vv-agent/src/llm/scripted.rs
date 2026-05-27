use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::types::LLMResponse;

use super::{LlmClient, LlmError, LlmRequest};

pub type ScriptStepCallback =
    Arc<dyn Fn(&LlmRequest) -> Result<LLMResponse, LlmError> + Send + Sync + 'static>;

#[derive(Clone)]
pub enum ScriptStep {
    Response(LLMResponse),
    Callback(ScriptStepCallback),
}

impl ScriptStep {
    pub fn response(response: LLMResponse) -> Self {
        Self::Response(response)
    }

    pub fn callback(
        callback: impl Fn(&LlmRequest) -> Result<LLMResponse, LlmError> + Send + Sync + 'static,
    ) -> Self {
        Self::Callback(Arc::new(callback))
    }
}

impl From<LLMResponse> for ScriptStep {
    fn from(response: LLMResponse) -> Self {
        Self::Response(response)
    }
}

#[derive(Clone)]
pub struct ScriptedLlmClient {
    steps: Arc<Mutex<VecDeque<ScriptStep>>>,
}

impl ScriptedLlmClient {
    pub fn new(responses: Vec<LLMResponse>) -> Self {
        Self::from_steps(responses.into_iter().map(ScriptStep::from).collect())
    }

    pub fn from_steps(steps: Vec<ScriptStep>) -> Self {
        Self {
            steps: Arc::new(Mutex::new(VecDeque::from(steps))),
        }
    }

    pub fn push_response(&self, response: LLMResponse) {
        self.push_step(ScriptStep::Response(response));
    }

    pub fn push_step(&self, step: ScriptStep) {
        if let Ok(mut queue) = self.steps.lock() {
            queue.push_back(step);
        }
    }
}

impl std::fmt::Debug for ScriptedLlmClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedLlmClient").finish_non_exhaustive()
    }
}

impl LlmClient for ScriptedLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let mut queue = self
            .steps
            .lock()
            .map_err(|_| LlmError::Request("scripted response queue poisoned".to_string()))?;
        let Some(step) = queue.pop_front() else {
            return Err(LlmError::ScriptExhausted);
        };
        drop(queue);
        match step {
            ScriptStep::Response(response) => Ok(response),
            ScriptStep::Callback(callback) => callback(&request),
        }
    }
}
