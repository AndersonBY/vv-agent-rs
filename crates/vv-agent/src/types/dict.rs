use super::*;

impl Message {
    pub fn to_openai_message(&self, include_reasoning_content: bool) -> Value {
        let mut payload = serde_json::Map::from_iter([
            (
                "role".to_string(),
                Value::String(message_role_value(self.role).to_string()),
            ),
            ("content".to_string(), Value::String(self.content.clone())),
        ]);
        insert_optional_string(&mut payload, "name", &self.name);
        insert_optional_string(&mut payload, "tool_call_id", &self.tool_call_id);
        if self.role == MessageRole::Assistant && !self.tool_calls.is_empty() {
            payload.insert(
                "tool_calls".to_string(),
                Value::Array(self.tool_calls.iter().map(tool_call_to_openai).collect()),
            );
            if self.content.is_empty() {
                payload.insert("content".to_string(), Value::Null);
            }
        }
        if include_reasoning_content
            && self.role == MessageRole::Assistant
            && self.reasoning_content.is_some()
        {
            insert_optional_string(&mut payload, "reasoning_content", &self.reasoning_content);
        }
        if self.role == MessageRole::User {
            if let Some(image_url) = &self.image_url {
                let mut blocks = Vec::new();
                if !self.content.is_empty() {
                    blocks.push(serde_json::json!({
                        "type": "text",
                        "text": self.content,
                    }));
                }
                blocks.push(serde_json::json!({
                    "type": "image_url",
                    "image_url": {"url": image_url},
                }));
                payload.insert("content".to_string(), Value::Array(blocks));
            }
        }
        Value::Object(payload)
    }

    pub fn to_dict(&self) -> Value {
        let mut payload = serde_json::Map::from_iter([
            (
                "role".to_string(),
                Value::String(message_role_value(self.role).to_string()),
            ),
            ("content".to_string(), Value::String(self.content.clone())),
        ]);
        insert_optional_string(&mut payload, "name", &self.name);
        insert_optional_string(&mut payload, "tool_call_id", &self.tool_call_id);
        if !self.tool_calls.is_empty() {
            payload.insert(
                "tool_calls".to_string(),
                Value::Array(self.tool_calls.iter().map(ToolCall::to_dict).collect()),
            );
        }
        insert_optional_string(&mut payload, "reasoning_content", &self.reasoning_content);
        insert_optional_string(&mut payload, "image_url", &self.image_url);
        if !self.metadata.is_empty() {
            payload.insert("metadata".to_string(), metadata_to_value(&self.metadata));
        }
        Value::Object(payload)
    }

    pub fn from_dict(data: &Value) -> Result<Self, String> {
        let object = expect_object(data, "Message")?;
        let role = parse_message_role(read_required_string(object, "role")?)?;
        let tool_calls = read_array(object, "tool_calls")
            .unwrap_or(&[])
            .iter()
            .map(ToolCall::from_dict)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            role,
            content: read_string(object, "content").unwrap_or_default(),
            name: read_optional_string(object, "name"),
            tool_call_id: read_optional_string(object, "tool_call_id"),
            tool_calls,
            reasoning_content: read_optional_string(object, "reasoning_content"),
            image_url: read_optional_string(object, "image_url"),
            metadata: read_metadata(object, "metadata")?,
        })
    }
}

fn tool_call_to_openai(tool_call: &ToolCall) -> Value {
    let mut payload = serde_json::json!({
        "id": tool_call.id,
        "type": "function",
        "function": {
            "name": tool_call.name,
            "arguments": Value::Object(tool_call.arguments.clone().into_iter().collect()).to_string(),
        },
    });
    if let Some(extra_content) = &tool_call.extra_content {
        payload["extra_content"] = extra_content.clone();
    }
    payload
}

impl ToolCall {
    pub fn to_dict(&self) -> Value {
        let mut payload = serde_json::Map::from_iter([
            ("id".to_string(), Value::String(self.id.clone())),
            ("name".to_string(), Value::String(self.name.clone())),
            (
                "arguments".to_string(),
                Value::Object(self.arguments.clone().into_iter().collect()),
            ),
        ]);
        if let Some(extra_content) = &self.extra_content {
            payload.insert("extra_content".to_string(), extra_content.clone());
        }
        Value::Object(payload)
    }

    pub fn from_dict(data: &Value) -> Result<Self, String> {
        let object = expect_object(data, "ToolCall")?;
        Ok(Self {
            id: read_required_string(object, "id")?.to_string(),
            name: read_required_string(object, "name")?.to_string(),
            arguments: read_metadata(object, "arguments")?,
            extra_content: object
                .get("extra_content")
                .filter(|value| value.is_object())
                .cloned(),
        })
    }
}

impl ToolExecutionResult {
    pub fn to_dict(&self) -> Value {
        let mut payload = serde_json::Map::from_iter([
            (
                "tool_call_id".to_string(),
                Value::String(self.tool_call_id.clone()),
            ),
            ("content".to_string(), Value::String(self.content.clone())),
            (
                "directive".to_string(),
                Value::String(tool_directive_value(self.directive).to_string()),
            ),
            (
                "status".to_string(),
                Value::String(tool_result_legacy_status(self.status).to_string()),
            ),
            (
                "status_code".to_string(),
                Value::String(tool_result_status_value(self.status).to_string()),
            ),
        ]);
        insert_optional_string(&mut payload, "error_code", &self.error_code);
        if !self.metadata.is_empty() {
            payload.insert("metadata".to_string(), metadata_to_value(&self.metadata));
        }
        insert_optional_string(&mut payload, "image_url", &self.image_url);
        insert_optional_string(&mut payload, "image_path", &self.image_path);
        Value::Object(payload)
    }

    pub fn from_dict(data: &Value) -> Result<Self, String> {
        let object = expect_object(data, "ToolExecutionResult")?;
        let status = match read_optional_string(object, "status_code").as_deref() {
            Some(status_code) => parse_tool_result_status(status_code)?,
            None => parse_legacy_tool_result_status(
                read_optional_string(object, "status")
                    .as_deref()
                    .unwrap_or("success"),
            )?,
        };
        Ok(Self {
            tool_call_id: read_string(object, "tool_call_id").unwrap_or_default(),
            content: read_string(object, "content").unwrap_or_default(),
            status,
            directive: parse_tool_directive(
                read_optional_string(object, "directive")
                    .as_deref()
                    .unwrap_or("continue"),
            )?,
            error_code: read_optional_string(object, "error_code"),
            metadata: read_metadata(object, "metadata")?,
            image_url: read_optional_string(object, "image_url"),
            image_path: read_optional_string(object, "image_path"),
        })
    }
}

impl CycleRecord {
    pub fn to_dict(&self) -> Value {
        Value::Object(serde_json::Map::from_iter([
            ("index".to_string(), Value::from(self.index)),
            (
                "assistant_message".to_string(),
                Value::String(self.assistant_message.clone()),
            ),
            (
                "tool_calls".to_string(),
                Value::Array(self.tool_calls.iter().map(ToolCall::to_dict).collect()),
            ),
            (
                "tool_results".to_string(),
                Value::Array(
                    self.tool_results
                        .iter()
                        .map(ToolExecutionResult::to_dict)
                        .collect(),
                ),
            ),
            (
                "memory_compacted".to_string(),
                Value::Bool(self.memory_compacted),
            ),
            (
                "token_usage".to_string(),
                token_usage_to_dict(&self.token_usage),
            ),
        ]))
    }

    pub fn from_dict(data: &Value) -> Result<Self, String> {
        let object = expect_object(data, "CycleRecord")?;
        let tool_calls = read_array(object, "tool_calls")
            .unwrap_or(&[])
            .iter()
            .map(ToolCall::from_dict)
            .collect::<Result<Vec<_>, _>>()?;
        let tool_results = read_array(object, "tool_results")
            .unwrap_or(&[])
            .iter()
            .map(ToolExecutionResult::from_dict)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            index: read_u32(object, "index", 0),
            assistant_message: read_string(object, "assistant_message").unwrap_or_default(),
            tool_calls,
            tool_results,
            memory_compacted: read_bool(object, "memory_compacted", false),
            token_usage: object
                .get("token_usage")
                .map(token_usage_from_dict)
                .transpose()?
                .unwrap_or_default(),
        })
    }
}

impl AgentTask {
    pub fn to_dict(&self) -> Value {
        Value::Object(serde_json::Map::from_iter([
            ("task_id".to_string(), Value::String(self.task_id.clone())),
            ("model".to_string(), Value::String(self.model.clone())),
            (
                "system_prompt".to_string(),
                Value::String(self.system_prompt.clone()),
            ),
            (
                "user_prompt".to_string(),
                Value::String(self.user_prompt.clone()),
            ),
            ("max_cycles".to_string(), Value::from(self.max_cycles)),
            (
                "memory_compact_threshold".to_string(),
                Value::from(self.memory_compact_threshold),
            ),
            (
                "memory_threshold_percentage".to_string(),
                Value::from(self.memory_threshold_percentage),
            ),
            (
                "no_tool_policy".to_string(),
                Value::String(no_tool_policy_value(self.no_tool_policy).to_string()),
            ),
            (
                "allow_interruption".to_string(),
                Value::Bool(self.allow_interruption),
            ),
            ("use_workspace".to_string(), Value::Bool(self.use_workspace)),
            (
                "has_sub_agents".to_string(),
                Value::Bool(self.has_sub_agents),
            ),
            (
                "sub_agents".to_string(),
                serde_json::to_value(&self.sub_agents).unwrap_or(Value::Null),
            ),
            (
                "agent_type".to_string(),
                self.agent_type
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            ),
            (
                "native_multimodal".to_string(),
                Value::Bool(self.native_multimodal),
            ),
            (
                "extra_tool_names".to_string(),
                string_vec_to_value(&self.extra_tool_names),
            ),
            (
                "exclude_tools".to_string(),
                string_vec_to_value(&self.exclude_tools),
            ),
            (
                "initial_messages".to_string(),
                Value::Array(self.initial_messages.iter().map(Message::to_dict).collect()),
            ),
            (
                "initial_shared_state".to_string(),
                metadata_to_value(&self.initial_shared_state),
            ),
            ("metadata".to_string(), metadata_to_value(&self.metadata)),
        ]))
    }

    pub fn from_dict(data: &Value) -> Result<Self, String> {
        let object = expect_object(data, "AgentTask")?;
        let mut task = Self::new(
            read_required_string(object, "task_id")?,
            read_required_string(object, "model")?,
            read_required_string(object, "system_prompt")?,
            read_required_string(object, "user_prompt")?,
        );
        task.max_cycles = read_u32(object, "max_cycles", 8);
        task.memory_compact_threshold = read_u64(object, "memory_compact_threshold", 128_000);
        task.memory_threshold_percentage = read_u8(object, "memory_threshold_percentage", 90);
        task.no_tool_policy = parse_no_tool_policy(
            read_optional_string(object, "no_tool_policy")
                .as_deref()
                .unwrap_or("continue"),
        )?;
        task.allow_interruption = read_bool(object, "allow_interruption", true);
        task.use_workspace = read_bool(object, "use_workspace", true);
        task.has_sub_agents = read_bool(object, "has_sub_agents", false);
        if let Some(value) = object.get("sub_agents") {
            task.sub_agents =
                serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
        }
        task.agent_type = read_optional_string(object, "agent_type");
        task.native_multimodal = read_bool(object, "native_multimodal", false);
        task.extra_tool_names = read_string_list(object, "extra_tool_names");
        task.exclude_tools = read_string_list(object, "exclude_tools");
        task.initial_messages = read_array(object, "initial_messages")
            .unwrap_or(&[])
            .iter()
            .map(Message::from_dict)
            .collect::<Result<Vec<_>, _>>()?;
        task.initial_shared_state = read_metadata(object, "initial_shared_state")?;
        task.metadata = read_metadata(object, "metadata")?;
        Ok(task)
    }
}

impl AgentResult {
    pub fn to_dict(&self) -> Value {
        Value::Object(serde_json::Map::from_iter([
            (
                "status".to_string(),
                Value::String(agent_status_value(self.status).to_string()),
            ),
            (
                "messages".to_string(),
                Value::Array(self.messages.iter().map(Message::to_dict).collect()),
            ),
            (
                "cycles".to_string(),
                Value::Array(self.cycles.iter().map(CycleRecord::to_dict).collect()),
            ),
            (
                "final_answer".to_string(),
                self.final_answer
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            ),
            (
                "wait_reason".to_string(),
                self.wait_reason
                    .clone()
                    .map(Value::String)
                    .unwrap_or(Value::Null),
            ),
            (
                "error".to_string(),
                self.error.clone().map(Value::String).unwrap_or(Value::Null),
            ),
            (
                "shared_state".to_string(),
                metadata_to_value(&self.shared_state),
            ),
            (
                "token_usage".to_string(),
                task_token_usage_to_dict(&self.token_usage),
            ),
        ]))
    }

    pub fn from_dict(data: &Value) -> Result<Self, String> {
        let object = expect_object(data, "AgentResult")?;
        let messages = read_array(object, "messages")
            .unwrap_or(&[])
            .iter()
            .map(Message::from_dict)
            .collect::<Result<Vec<_>, _>>()?;
        let cycles = read_array(object, "cycles")
            .unwrap_or(&[])
            .iter()
            .map(CycleRecord::from_dict)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            status: parse_agent_status(read_required_string(object, "status")?)?,
            messages,
            cycles,
            final_answer: read_optional_string(object, "final_answer"),
            wait_reason: read_optional_string(object, "wait_reason"),
            error: read_optional_string(object, "error"),
            shared_state: read_metadata(object, "shared_state")?,
            token_usage: object
                .get("token_usage")
                .map(task_token_usage_from_dict)
                .transpose()?
                .unwrap_or_default(),
        })
    }
}

fn expect_object<'a>(
    value: &'a Value,
    type_name: &str,
) -> Result<&'a serde_json::Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{type_name} payload must be an object"))
}

fn read_required_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing required string field {key:?}"))
}

fn read_string(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_string)
}

fn read_optional_string(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .filter(|value| !value.is_null())
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn read_bool(object: &serde_json::Map<String, Value>, key: &str, default: bool) -> bool {
    object.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn read_u32(object: &serde_json::Map<String, Value>, key: &str, default: u32) -> u32 {
    object
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(default)
}

fn read_u64(object: &serde_json::Map<String, Value>, key: &str, default: u64) -> u64 {
    object.get(key).and_then(Value::as_u64).unwrap_or(default)
}

fn read_u8(object: &serde_json::Map<String, Value>, key: &str, default: u8) -> u8 {
    object
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u8::try_from(value).ok())
        .unwrap_or(default)
}

fn read_array<'a>(object: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a [Value]> {
    object.get(key).and_then(Value::as_array).map(Vec::as_slice)
}

fn read_metadata(object: &serde_json::Map<String, Value>, key: &str) -> Result<Metadata, String> {
    match object.get(key) {
        Some(Value::Object(map)) => Ok(map.clone().into_iter().collect()),
        Some(Value::Null) | None => Ok(Metadata::new()),
        Some(_) => Err(format!("{key:?} must be an object")),
    }
}

fn read_string_list(object: &serde_json::Map<String, Value>, key: &str) -> Vec<String> {
    object
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn insert_optional_string(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
    value: &Option<String>,
) {
    if let Some(value) = value {
        object.insert(key.to_string(), Value::String(value.clone()));
    }
}

fn metadata_to_value(metadata: &Metadata) -> Value {
    Value::Object(metadata.clone().into_iter().collect())
}

fn string_vec_to_value(items: &[String]) -> Value {
    Value::Array(items.iter().cloned().map(Value::String).collect())
}

fn message_role_value(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

fn parse_message_role(value: &str) -> Result<MessageRole, String> {
    match value {
        "system" => Ok(MessageRole::System),
        "user" => Ok(MessageRole::User),
        "assistant" => Ok(MessageRole::Assistant),
        "tool" => Ok(MessageRole::Tool),
        other => Err(format!("unknown message role: {other}")),
    }
}

fn tool_directive_value(directive: ToolDirective) -> &'static str {
    match directive {
        ToolDirective::Continue => "continue",
        ToolDirective::WaitUser => "wait_user",
        ToolDirective::Finish => "finish",
    }
}

fn parse_tool_directive(value: &str) -> Result<ToolDirective, String> {
    match value {
        "continue" => Ok(ToolDirective::Continue),
        "wait_user" => Ok(ToolDirective::WaitUser),
        "finish" => Ok(ToolDirective::Finish),
        other => Err(format!("unknown tool directive: {other}")),
    }
}

fn tool_result_status_value(status: ToolResultStatus) -> &'static str {
    match status {
        ToolResultStatus::Success => "SUCCESS",
        ToolResultStatus::Error => "ERROR",
        ToolResultStatus::WaitResponse => "WAIT_RESPONSE",
        ToolResultStatus::Running => "RUNNING",
        ToolResultStatus::PendingCompress => "PENDING_COMPRESS",
    }
}

fn parse_tool_result_status(value: &str) -> Result<ToolResultStatus, String> {
    match value {
        "SUCCESS" => Ok(ToolResultStatus::Success),
        "ERROR" => Ok(ToolResultStatus::Error),
        "WAIT_RESPONSE" => Ok(ToolResultStatus::WaitResponse),
        "RUNNING" => Ok(ToolResultStatus::Running),
        "PENDING_COMPRESS" => Ok(ToolResultStatus::PendingCompress),
        other => Err(format!("unknown tool result status: {other}")),
    }
}

fn parse_legacy_tool_result_status(value: &str) -> Result<ToolResultStatus, String> {
    match value {
        "success" => Ok(ToolResultStatus::Success),
        "error" => Ok(ToolResultStatus::Error),
        other => Err(format!("unknown legacy tool result status: {other}")),
    }
}

fn tool_result_legacy_status(status: ToolResultStatus) -> &'static str {
    match status {
        ToolResultStatus::Error => "error",
        ToolResultStatus::Success
        | ToolResultStatus::WaitResponse
        | ToolResultStatus::Running
        | ToolResultStatus::PendingCompress => "success",
    }
}

fn no_tool_policy_value(policy: NoToolPolicy) -> &'static str {
    match policy {
        NoToolPolicy::Continue => "continue",
        NoToolPolicy::WaitUser => "wait_user",
        NoToolPolicy::Finish => "finish",
    }
}

fn parse_no_tool_policy(value: &str) -> Result<NoToolPolicy, String> {
    match value {
        "continue" => Ok(NoToolPolicy::Continue),
        "wait_user" => Ok(NoToolPolicy::WaitUser),
        "finish" => Ok(NoToolPolicy::Finish),
        other => Err(format!("unknown no_tool_policy: {other}")),
    }
}

fn agent_status_value(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Pending => "pending",
        AgentStatus::Running => "running",
        AgentStatus::WaitUser => "wait_user",
        AgentStatus::Completed => "completed",
        AgentStatus::Failed => "failed",
        AgentStatus::MaxCycles => "max_cycles",
    }
}

fn parse_agent_status(value: &str) -> Result<AgentStatus, String> {
    match value {
        "pending" => Ok(AgentStatus::Pending),
        "running" => Ok(AgentStatus::Running),
        "wait_user" => Ok(AgentStatus::WaitUser),
        "completed" => Ok(AgentStatus::Completed),
        "failed" => Ok(AgentStatus::Failed),
        "max_cycles" => Ok(AgentStatus::MaxCycles),
        other => Err(format!("unknown agent status: {other}")),
    }
}

fn token_usage_to_dict(usage: &TokenUsage) -> Value {
    Value::Object(serde_json::Map::from_iter([
        (
            "prompt_tokens".to_string(),
            Value::from(usage.prompt_tokens),
        ),
        (
            "completion_tokens".to_string(),
            Value::from(usage.completion_tokens),
        ),
        ("total_tokens".to_string(), Value::from(usage.total_tokens)),
        (
            "cached_tokens".to_string(),
            Value::from(usage.cached_tokens),
        ),
        (
            "reasoning_tokens".to_string(),
            Value::from(usage.reasoning_tokens),
        ),
        ("input_tokens".to_string(), Value::from(usage.input_tokens)),
        (
            "output_tokens".to_string(),
            Value::from(usage.output_tokens),
        ),
        (
            "cache_creation_tokens".to_string(),
            Value::from(usage.cache_creation_tokens),
        ),
        ("raw".to_string(), usage.raw.clone()),
    ]))
}

fn token_usage_from_dict(value: &Value) -> Result<TokenUsage, String> {
    let object = expect_object(value, "TokenUsage")?;
    Ok(TokenUsage {
        prompt_tokens: read_u64(object, "prompt_tokens", 0),
        completion_tokens: read_u64(object, "completion_tokens", 0),
        total_tokens: read_u64(object, "total_tokens", 0),
        cached_tokens: read_u64(object, "cached_tokens", 0),
        reasoning_tokens: read_u64(object, "reasoning_tokens", 0),
        input_tokens: read_u64(object, "input_tokens", 0),
        output_tokens: read_u64(object, "output_tokens", 0),
        cache_creation_tokens: read_u64(object, "cache_creation_tokens", 0),
        raw: object
            .get("raw")
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default())),
    })
}

fn task_token_usage_to_dict(usage: &TaskTokenUsage) -> Value {
    Value::Object(serde_json::Map::from_iter([
        (
            "prompt_tokens".to_string(),
            Value::from(usage.prompt_tokens),
        ),
        (
            "completion_tokens".to_string(),
            Value::from(usage.completion_tokens),
        ),
        ("total_tokens".to_string(), Value::from(usage.total_tokens)),
        (
            "cached_tokens".to_string(),
            Value::from(usage.cached_tokens),
        ),
        (
            "reasoning_tokens".to_string(),
            Value::from(usage.reasoning_tokens),
        ),
        ("input_tokens".to_string(), Value::from(usage.input_tokens)),
        (
            "output_tokens".to_string(),
            Value::from(usage.output_tokens),
        ),
        (
            "cache_creation_tokens".to_string(),
            Value::from(usage.cache_creation_tokens),
        ),
        (
            "cycles".to_string(),
            Value::Array(
                usage
                    .cycles
                    .iter()
                    .map(|cycle| {
                        let mut payload = match token_usage_to_dict(&cycle.usage) {
                            Value::Object(map) => map,
                            _ => serde_json::Map::new(),
                        };
                        payload.insert("cycle_index".to_string(), Value::from(cycle.cycle_index));
                        Value::Object(payload)
                    })
                    .collect(),
            ),
        ),
    ]))
}

fn task_token_usage_from_dict(value: &Value) -> Result<TaskTokenUsage, String> {
    let object = expect_object(value, "TaskTokenUsage")?;
    Ok(TaskTokenUsage {
        prompt_tokens: read_u64(object, "prompt_tokens", 0),
        completion_tokens: read_u64(object, "completion_tokens", 0),
        total_tokens: read_u64(object, "total_tokens", 0),
        cached_tokens: read_u64(object, "cached_tokens", 0),
        reasoning_tokens: read_u64(object, "reasoning_tokens", 0),
        input_tokens: read_u64(object, "input_tokens", 0),
        output_tokens: read_u64(object, "output_tokens", 0),
        cache_creation_tokens: read_u64(object, "cache_creation_tokens", 0),
        cycles: Vec::new(),
    })
}
