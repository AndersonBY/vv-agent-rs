use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSettings {
    #[serde(
        default,
        with = "temperature_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub temperature: Option<f64>,
    #[serde(
        default,
        with = "top_p_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub top_p: Option<f64>,
    #[serde(
        default,
        alias = "max_output_tokens",
        with = "positive_u32_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    #[serde(
        default,
        with = "reasoning_option",
        skip_serializing_if = "reasoning_option::is_none_or_empty"
    )]
    pub reasoning: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    #[serde(
        default,
        rename = "timeout_seconds",
        with = "duration_seconds_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub timeout: Option<Duration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetrySettings>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra_headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub extra_body: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub extra_args: Map<String, Value>,
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
        if override_settings.max_tokens.is_some() {
            merged.max_tokens = override_settings.max_tokens;
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
            .extra_args
            .extend(override_settings.extra_args.clone());
        merged
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_finite_min("temperature", self.temperature, 0.0, false)?;
        validate_finite_range("top_p", self.top_p, 0.0, 1.0)?;
        if self.max_tokens == Some(0) {
            return Err("max_tokens must be greater than zero".to_string());
        }
        if self.timeout.is_some_and(|timeout| timeout.is_zero()) {
            return Err("timeout_seconds must be greater than zero".to_string());
        }
        if let Some(retry) = self.retry.as_ref() {
            retry.validate()?;
        }
        if self.tool_choice.as_ref().is_some_and(
            |choice| matches!(choice, ToolChoice::Tool(name) if name.trim().is_empty()),
        ) {
            return Err("named tool_choice requires a non-empty function name".to_string());
        }
        if self
            .reasoning
            .as_ref()
            .is_some_and(|value| !value.is_object())
        {
            return Err("reasoning must be an object".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct ModelSettingsBuilder {
    settings: ModelSettings,
}

impl ModelSettingsBuilder {
    pub fn temperature(mut self, temperature: f64) -> Self {
        self.settings.temperature = Some(temperature);
        self
    }

    pub fn top_p(mut self, top_p: f64) -> Self {
        self.settings.top_p = Some(top_p);
        self
    }

    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.settings.max_tokens = Some(max_tokens);
        self
    }

    pub fn max_output_tokens(self, max_output_tokens: u32) -> Self {
        self.max_tokens(max_output_tokens)
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
        self.settings.reasoning =
            (!reasoning.as_object().is_some_and(Map::is_empty)).then_some(reasoning);
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

    pub fn retry(mut self, retry: RetrySettings) -> Self {
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

    pub fn extra_arg(mut self, key: impl Into<String>, value: Value) -> Self {
        self.settings.extra_args.insert(key.into(), value);
        self
    }

    pub fn build(self) -> ModelSettings {
        self.settings
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Tool(String),
}

impl Serialize for ToolChoice {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Auto => serializer.serialize_str("auto"),
            Self::None => serializer.serialize_str("none"),
            Self::Required => serializer.serialize_str("required"),
            Self::Tool(name) => {
                if name.trim().is_empty() {
                    return Err(serde::ser::Error::custom(
                        "named tool_choice requires a non-empty function name",
                    ));
                }
                serde_json::json!({
                    "type": "function",
                    "function": {"name": name},
                })
                .serialize(serializer)
            }
        }
    }
}

impl<'de> Deserialize<'de> for ToolChoice {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        if let Some(mode) = value.as_str() {
            return match mode {
                "auto" => Ok(Self::Auto),
                "none" => Ok(Self::None),
                "required" => Ok(Self::Required),
                _ => Err(serde::de::Error::custom(format!(
                    "unknown tool_choice mode: {mode}"
                ))),
            };
        }
        let object = value.as_object().ok_or_else(|| {
            serde::de::Error::custom("tool_choice must be a mode or function object")
        })?;
        if object.len() != 2 || object.get("type") != Some(&Value::String("function".to_string())) {
            return Err(serde::de::Error::custom(
                "named tool_choice must use the standard function object",
            ));
        }
        let function = object
            .get("function")
            .and_then(Value::as_object)
            .filter(|function| function.len() == 1)
            .ok_or_else(|| {
                serde::de::Error::custom("tool_choice function must contain only name")
            })?;
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| serde::de::Error::custom("tool_choice function name cannot be empty"))?;
        Ok(Self::Tool(name.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    Text,
    JsonObject,
    JsonSchema { json_schema: Map<String, Value> },
}

impl<'de> Deserialize<'de> for ResponseFormat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let object = value
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("response_format must be an object"))?;
        let format_type = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| serde::de::Error::custom("response_format.type must be a string"))?;
        match format_type {
            "text" if object.len() == 1 => Ok(Self::Text),
            "json_object" if object.len() == 1 => Ok(Self::JsonObject),
            "json_schema" if object.len() == 2 => {
                let json_schema = object
                    .get("json_schema")
                    .and_then(Value::as_object)
                    .cloned()
                    .ok_or_else(|| {
                        serde::de::Error::custom("json_schema response_format requires an object")
                    })?;
                Ok(Self::JsonSchema { json_schema })
            }
            _ => Err(serde::de::Error::custom(
                "invalid or unsupported response_format wire shape",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RetrySettings {
    pub max_attempts: u32,
    pub backoff_seconds: f64,
}

impl RetrySettings {
    pub fn new(max_attempts: u32) -> Self {
        Self {
            max_attempts,
            backoff_seconds: 2.0,
        }
    }

    pub fn with_backoff_seconds(mut self, backoff_seconds: f64) -> Self {
        self.backoff_seconds = backoff_seconds;
        self
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.max_attempts == 0 {
            return Err("retry.max_attempts must be greater than zero".to_string());
        }
        if !self.backoff_seconds.is_finite() || self.backoff_seconds < 0.0 {
            return Err("retry.backoff_seconds must be a finite non-negative number".to_string());
        }
        Ok(())
    }
}

impl Default for RetrySettings {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            backoff_seconds: 2.0,
        }
    }
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RetrySettingsWire {
    max_attempts: u32,
    backoff_seconds: f64,
}

impl Default for RetrySettingsWire {
    fn default() -> Self {
        let defaults = RetrySettings::default();
        Self {
            max_attempts: defaults.max_attempts,
            backoff_seconds: defaults.backoff_seconds,
        }
    }
}

impl<'de> Deserialize<'de> for RetrySettings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = RetrySettingsWire::deserialize(deserializer)?;
        let settings = Self {
            max_attempts: wire.max_attempts,
            backoff_seconds: wire.backoff_seconds,
        };
        settings.validate().map_err(serde::de::Error::custom)?;
        Ok(settings)
    }
}

pub type RetryPolicy = RetrySettings;

mod duration_seconds_option {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(value) => serializer.serialize_some(&value.as_secs_f64()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let seconds = Option::<f64>::deserialize(deserializer)?;
        seconds
            .map(|seconds| {
                if seconds.is_finite() && seconds > 0.0 {
                    Ok(Duration::from_secs_f64(seconds))
                } else {
                    Err(serde::de::Error::custom(
                        "timeout_seconds must be a finite positive number",
                    ))
                }
            })
            .transpose()
    }
}

macro_rules! finite_option_module {
    ($module:ident, $validator:expr) => {
        mod $module {
            use serde::{Deserialize, Deserializer, Serialize, Serializer};

            pub fn serialize<S>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                value.serialize(serializer)
            }

            pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = Option::<f64>::deserialize(deserializer)?;
                if value.is_some_and(|value| !($validator)(value)) {
                    return Err(serde::de::Error::custom(concat!(
                        stringify!($module),
                        " is outside its valid range"
                    )));
                }
                Ok(value)
            }
        }
    };
}

finite_option_module!(temperature_option, |value: f64| value.is_finite()
    && value >= 0.0);
finite_option_module!(top_p_option, |value: f64| value.is_finite()
    && (0.0..=1.0).contains(&value));

mod positive_u32_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(value: &Option<u32>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        value.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Option::<u32>::deserialize(deserializer)?;
        if value == Some(0) {
            return Err(serde::de::Error::custom(
                "max_tokens must be greater than zero",
            ));
        }
        Ok(value)
    }
}

mod reasoning_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use serde_json::{Map, Value};

    pub fn is_none_or_empty(value: &Option<Value>) -> bool {
        value
            .as_ref()
            .is_none_or(|value| value.as_object().is_some_and(Map::is_empty))
    }

    pub fn serialize<S>(value: &Option<Value>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        value.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Option::<Value>::deserialize(deserializer)?;
        match value {
            Some(Value::Object(object)) if object.is_empty() => Ok(None),
            Some(value @ Value::Object(_)) => Ok(Some(value)),
            Some(_) => Err(serde::de::Error::custom("reasoning must be an object")),
            None => Ok(None),
        }
    }
}

fn validate_finite_min(
    name: &str,
    value: Option<f64>,
    minimum: f64,
    exclusive: bool,
) -> Result<(), String> {
    let Some(value) = value else {
        return Ok(());
    };
    if !value.is_finite() || (exclusive && value <= minimum) || (!exclusive && value < minimum) {
        return Err(format!("{name} is outside its valid range"));
    }
    Ok(())
}

fn validate_finite_range(
    name: &str,
    value: Option<f64>,
    minimum: f64,
    maximum: f64,
) -> Result<(), String> {
    validate_finite_min(name, value, minimum, false)?;
    if value.is_some_and(|value| value > maximum) {
        return Err(format!("{name} is outside its valid range"));
    }
    Ok(())
}
