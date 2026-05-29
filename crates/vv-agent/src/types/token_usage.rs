use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cached_tokens: u64,
    pub reasoning_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub raw: Value,
}

impl TokenUsage {
    pub fn has_usage(&self) -> bool {
        self.prompt_tokens > 0
            || self.completion_tokens > 0
            || self.total_tokens > 0
            || self.cached_tokens > 0
            || self.reasoning_tokens > 0
            || self.input_tokens > 0
            || self.output_tokens > 0
            || self.cache_creation_tokens > 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CycleTokenUsage {
    pub cycle_index: u32,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskTokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub cached_tokens: u64,
    pub reasoning_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cycles: Vec<CycleTokenUsage>,
}

impl TaskTokenUsage {
    pub fn add_cycle(&mut self, cycle_index: u32, usage: TokenUsage) {
        if !usage.has_usage() {
            return;
        }
        self.prompt_tokens += usage.prompt_tokens;
        self.completion_tokens += usage.completion_tokens;
        self.total_tokens += usage.total_tokens;
        self.cached_tokens += usage.cached_tokens;
        self.reasoning_tokens += usage.reasoning_tokens;
        self.input_tokens += usage.input_tokens;
        self.output_tokens += usage.output_tokens;
        self.cache_creation_tokens += usage.cache_creation_tokens;
        self.cycles.push(CycleTokenUsage { cycle_index, usage });
    }
}
