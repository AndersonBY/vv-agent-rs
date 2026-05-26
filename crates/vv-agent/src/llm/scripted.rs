use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::types::LLMResponse;

use super::{LlmClient, LlmError, LlmRequest};

#[derive(Clone)]
pub struct ScriptedLlmClient {
    responses: Arc<Mutex<VecDeque<LLMResponse>>>,
}

impl ScriptedLlmClient {
    pub fn new(responses: Vec<LLMResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::from(responses))),
        }
    }

    pub fn push_response(&self, response: LLMResponse) {
        if let Ok(mut queue) = self.responses.lock() {
            queue.push_back(response);
        }
    }
}

impl std::fmt::Debug for ScriptedLlmClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedLlmClient").finish_non_exhaustive()
    }
}

impl LlmClient for ScriptedLlmClient {
    fn complete(&self, _request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let mut queue = self
            .responses
            .lock()
            .map_err(|_| LlmError::Request("scripted response queue poisoned".to_string()))?;
        Ok(queue.pop_front().unwrap_or_else(|| LLMResponse::new("")))
    }
}
