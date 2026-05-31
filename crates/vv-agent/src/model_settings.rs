use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelSettings {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub tool_choice: Option<ToolChoice>,
    pub parallel_tool_calls: Option<bool>,
    pub reasoning: Option<Value>,
    pub response_format: Option<ResponseFormat>,
    #[serde(skip)]
    pub timeout: Option<Duration>,
    pub retry: Option<RetryPolicy>,
    pub extra_headers: BTreeMap<String, String>,
    pub extra_body: Map<String, Value>,
}

impl ModelSettings {
    pub fn builder() -> ModelSettingsBuilder {
        ModelSettingsBuilder::default()
    }

    pub fn merge(&self, override_settings: &ModelSettings) -> ModelSettings {
        let mut merged = self.clone();
        if override_settings.temperature.is_some() {
            merged.temperature = override_settings.temperature;
        }
        if override_settings.top_p.is_some() {
            merged.top_p = override_settings.top_p;
        }
        if override_settings.max_output_tokens.is_some() {
            merged.max_output_tokens = override_settings.max_output_tokens;
        }
        if override_settings.tool_choice.is_some() {
            merged.tool_choice = override_settings.tool_choice.clone();
        }
        if override_settings.parallel_tool_calls.is_some() {
            merged.parallel_tool_calls = override_settings.parallel_tool_calls;
        }
        if override_settings.reasoning.is_some() {
            merged.reasoning = override_settings.reasoning.clone();
        }
        if override_settings.response_format.is_some() {
            merged.response_format = override_settings.response_format.clone();
        }
        if override_settings.timeout.is_some() {
            merged.timeout = override_settings.timeout;
        }
        if override_settings.retry.is_some() {
            merged.retry = override_settings.retry.clone();
        }
        merged
            .extra_headers
            .extend(override_settings.extra_headers.clone());
        merged
            .extra_body
            .extend(override_settings.extra_body.clone());
        merged
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ModelSettingsBuilder {
    settings: ModelSettings,
}

impl ModelSettingsBuilder {
    pub fn temperature(mut self, temperature: f32) -> Self {
        self.settings.temperature = Some(temperature);
        self
    }

    pub fn top_p(mut self, top_p: f32) -> Self {
        self.settings.top_p = Some(top_p);
        self
    }

    pub fn max_output_tokens(mut self, max_output_tokens: u32) -> Self {
        self.settings.max_output_tokens = Some(max_output_tokens);
        self
    }

    pub fn tool_choice(mut self, tool_choice: ToolChoice) -> Self {
        self.settings.tool_choice = Some(tool_choice);
        self
    }

    pub fn parallel_tool_calls(mut self, parallel_tool_calls: bool) -> Self {
        self.settings.parallel_tool_calls = Some(parallel_tool_calls);
        self
    }

    pub fn reasoning(mut self, reasoning: Value) -> Self {
        self.settings.reasoning = Some(reasoning);
        self
    }

    pub fn response_format(mut self, response_format: ResponseFormat) -> Self {
        self.settings.response_format = Some(response_format);
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.settings.timeout = Some(timeout);
        self
    }

    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.settings.retry = Some(retry);
        self
    }

    pub fn extra_header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.settings.extra_headers.insert(key.into(), value.into());
        self
    }

    pub fn extra_body(mut self, key: impl Into<String>, value: Value) -> Self {
        self.settings.extra_body.insert(key.into(), value);
        self
    }

    pub fn build(self) -> ModelSettings {
        self.settings
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Tool(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    Text,
    JsonObject,
    JsonSchema { schema: Value },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_retries: u32,
}

impl RetryPolicy {
    pub fn new(max_retries: u32) -> Self {
        Self { max_retries }
    }
}
