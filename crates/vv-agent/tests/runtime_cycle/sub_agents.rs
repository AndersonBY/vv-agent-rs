use super::*;

struct ConstantHostCostMeter;

impl HostCostMeter for ConstantHostCostMeter {
    fn read(&self) -> Result<Option<HostCost>, String> {
        Ok(Some(HostCost::new("credits", 0).expect("host cost")))
    }
}

#[test]
fn runtime_executes_configured_sub_agent_with_real_runner() {
    let mut sub_task_args = BTreeMap::new();
    sub_task_args.insert("agent_id".to_string(), json!("researcher"));
    sub_task_args.insert(
        "task_description".to_string(),
        json!("Find the target crate"),
    );
    let mut child_finish_args = BTreeMap::new();
    child_finish_args.insert("message".to_string(), json!("child found vv-llm"));
    let mut parent_finish_args = BTreeMap::new();
    parent_finish_args.insert("message".to_string(), json!("parent saw child result"));

    let llm = ScriptedLlmClient::new(vec![
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "parent_sub_call",
                "create_sub_task",
                sub_task_args,
            )],
        ),
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "child_finish",
                "task_finish",
                child_finish_args,
            )],
        ),
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "parent_finish",
                "task_finish",
                parent_finish_args,
            )],
        ),
    ]);
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new("parent", "demo", "parent system", "delegate");
    task.sub_agents.insert(
        "researcher".to_string(),
        SubAgentConfig::new("demo", "research profile"),
    );

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.final_answer.as_deref(),
        Some("parent saw child result")
    );
    let sub_task_result = result
        .cycles
        .iter()
        .flat_map(|cycle| &cycle.tool_results)
        .find(|tool_result| tool_result.tool_call_id == "parent_sub_call")
        .expect("sub-task tool result");
    assert_eq!(sub_task_result.status, vv_agent::ToolResultStatus::Success);
    let payload: serde_json::Value =
        serde_json::from_str(&sub_task_result.content).expect("sub-task payload");
    assert_eq!(payload["status"], "completed");
    assert_eq!(payload["agent_name"], "researcher");
    assert_eq!(payload["final_answer"], "child found vv-llm");
}

#[test]
fn configured_sub_agent_inherits_limits_with_fresh_counters_and_without_parent_meter() {
    let mut sub_task_args = BTreeMap::new();
    sub_task_args.insert("agent_id".to_string(), json!("researcher"));
    sub_task_args.insert(
        "task_description".to_string(),
        json!("Find the target crate"),
    );
    let mut child_finish_args = BTreeMap::new();
    child_finish_args.insert("message".to_string(), json!("child done"));
    let mut parent_finish_args = BTreeMap::new();
    parent_finish_args.insert("message".to_string(), json!("parent done"));

    let mut parent_delegate = LLMResponse::with_tool_calls(
        "delegate",
        vec![ToolCall::new(
            "parent_sub_call",
            "create_sub_task",
            sub_task_args,
        )],
    );
    parent_delegate.token_usage = TokenUsage {
        total_tokens: 4,
        usage_source: UsageSource::ProviderReported,
        ..TokenUsage::default()
    };
    let mut child_finish = LLMResponse::with_tool_calls(
        "child work",
        vec![ToolCall::new(
            "child_finish",
            "task_finish",
            child_finish_args,
        )],
    );
    child_finish.token_usage = TokenUsage {
        total_tokens: 4,
        usage_source: UsageSource::ProviderReported,
        ..TokenUsage::default()
    };
    let mut parent_finish = LLMResponse::with_tool_calls(
        "parent synthesis",
        vec![ToolCall::new(
            "parent_finish",
            "task_finish",
            parent_finish_args,
        )],
    );
    parent_finish.token_usage = TokenUsage {
        total_tokens: 1,
        usage_source: UsageSource::ProviderReported,
        ..TokenUsage::default()
    };
    let runtime = AgentRuntime::new(ScriptedLlmClient::new(vec![
        parent_delegate,
        child_finish,
        parent_finish,
    ]));
    let mut task = AgentTask::new("parent-budget", "demo", "parent system", "delegate");
    task.sub_agents.insert(
        "researcher".to_string(),
        SubAgentConfig::new("demo", "research profile"),
    );
    let child_events = Arc::new(Mutex::new(Vec::<BTreeMap<String, serde_json::Value>>::new()));
    let child_events_for_handler = child_events.clone();
    let limits = RunBudgetLimits::builder()
        .max_total_tokens(5)
        .max_host_cost(HostCost::new("credits", 100).expect("host limit"))
        .build()
        .expect("budget limits");

    let result = runtime
        .run_with_controls(
            task,
            RuntimeRunControls {
                log_handler: Some(Arc::new(move |event, payload| {
                    if event == "sub_run_completed" {
                        child_events_for_handler
                            .lock()
                            .expect("child events")
                            .push(payload.clone());
                    }
                })),
                budget_limits: Some(limits),
                host_cost_meter: Some(Arc::new(ConstantHostCostMeter)),
                ..RuntimeRunControls::default()
            },
        )
        .expect("budgeted parent run");

    assert_eq!(result.status, AgentStatus::Completed);
    let parent_usage = result.budget_usage.expect("parent budget usage");
    assert_eq!(parent_usage.cycles, 2);
    assert_eq!(parent_usage.total_tokens, Some(5));
    assert_eq!(
        parent_usage
            .host_cost
            .as_ref()
            .map(|cost| cost.amount_microunits),
        Some(0)
    );
    let child_events = child_events.lock().expect("child events");
    assert_eq!(child_events.len(), 1);
    let child_usage = &child_events[0]["budget_usage"];
    assert_eq!(child_usage["cycles"], 1);
    assert_eq!(child_usage["total_tokens"], 4);
    assert_eq!(child_usage["host_cost"], serde_json::Value::Null);
    let host_unavailable = child_usage["unavailable_dimensions"]
        .as_array()
        .expect("unavailable dimensions")
        .iter()
        .find(|item| item["dimension"] == "host_cost")
        .expect("host cost unavailable");
    assert_eq!(host_unavailable["reason"], "meter_missing");
    assert!(!child_events[0].contains_key("budget_exhaustion"));
}

#[test]
fn runtime_forwards_stream_callback_to_runtime_backed_sub_agent() {
    let contract: serde_json::Value = serde_json::from_str(include_str!(
        "../fixtures/parity/configured_sub_agent_v1.json"
    ))
    .expect("configured sub-agent contract");
    assert!(contract["capability_projection"]["inherited"]
        .as_array()
        .expect("inherited capabilities")
        .contains(&json!("stream_sink")));
    let events = Arc::new(Mutex::new(Vec::new()));
    let stream_callback: LlmStreamCallback = {
        let events = Arc::clone(&events);
        Arc::new(move |event| {
            events.lock().expect("events").push(event.clone());
        })
    };
    let log_events = Arc::new(Mutex::new(Vec::<(
        String,
        BTreeMap<String, serde_json::Value>,
    )>::new()));
    let log_sink = Arc::clone(&log_events);
    let mut runtime = AgentRuntime::new(StreamingSubAgentLlmClient::default());
    runtime.log_handler = Some(Arc::new(Mutex::new(Box::new(
        move |event: &str, payload: &BTreeMap<String, serde_json::Value>| {
            log_sink
                .lock()
                .expect("log events")
                .push((event.to_string(), payload.clone()));
        },
    ))));
    let mut task = AgentTask::new("parent_stream", "demo", "parent system", "delegate");
    task.sub_agents.insert(
        "researcher".to_string(),
        SubAgentConfig::new("demo", "research profile"),
    );

    let result = runtime
        .run_with_controls(
            task,
            RuntimeRunControls {
                execution_context: Some(
                    ExecutionContext::default().with_stream_callback(stream_callback),
                ),
                ..RuntimeRunControls::default()
            },
        )
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert!(events.lock().expect("events").iter().any(|event| {
        event.get("event").and_then(serde_json::Value::as_str) == Some("assistant_delta")
            && event
                .get("content_delta")
                .and_then(serde_json::Value::as_str)
                == Some("checking")
            && event
                .get("sub_agent_name")
                .and_then(serde_json::Value::as_str)
                == Some("researcher")
            && event.get("task_id").and_then(serde_json::Value::as_str) != Some("spoofed-task")
            && event.get("session_id") == event.get("task_id")
    }));
    let log_events = log_events.lock().expect("log events");
    let log_event_names = log_events
        .iter()
        .map(|(event, _)| event.as_str())
        .collect::<Vec<_>>();
    assert!(log_event_names.contains(&"sub_agent_tool_call_started"));
    assert!(log_event_names.contains(&"sub_agent_tool_call_progress"));
    let sub_agent_delta = log_events
        .iter()
        .find(|(event, _)| event == "sub_agent_assistant_delta")
        .expect("sub-agent stream event in runtime logs");
    assert_eq!(sub_agent_delta.1["content_delta"], json!("checking"));
    assert_eq!(sub_agent_delta.1["sub_agent_name"], json!("researcher"));
    assert!(sub_agent_delta.1["task_id"].as_str().is_some());
    assert_ne!(sub_agent_delta.1["task_id"], json!("spoofed-task"));
    assert_eq!(
        sub_agent_delta.1["session_id"],
        sub_agent_delta.1["task_id"]
    );
    let sub_agent_progress = log_events
        .iter()
        .find(|(event, _)| event == "sub_agent_tool_call_progress")
        .expect("sub-agent tool progress event in runtime logs");
    assert_eq!(sub_agent_progress.1["tool_call_id"], json!("sub_tool_1"));
    assert_eq!(sub_agent_progress.1["function_name"], json!("bash"));
    assert_eq!(sub_agent_progress.1["arguments_chars"], json!(48));
    assert_eq!(sub_agent_progress.1["estimated_tokens"], json!(12));
    assert_eq!(sub_agent_progress.1["sub_agent_name"], json!("researcher"));
    assert!(sub_agent_progress.1["task_id"].as_str().is_some());
    assert_eq!(
        sub_agent_progress.1["session_id"],
        sub_agent_progress.1["task_id"]
    );
}

#[test]
fn runtime_rejects_sub_agent_model_mismatch_without_settings_file() {
    let mut sub_task_args = BTreeMap::new();
    sub_task_args.insert("agent_id".to_string(), json!("researcher"));
    sub_task_args.insert(
        "task_description".to_string(),
        json!("Use a different model"),
    );
    let mut parent_finish_args = BTreeMap::new();
    parent_finish_args.insert(
        "message".to_string(),
        json!("parent recorded child failure"),
    );

    let llm = ScriptedLlmClient::new(vec![
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "parent_sub_call",
                "create_sub_task",
                sub_task_args,
            )],
        ),
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "parent_finish",
                "task_finish",
                parent_finish_args,
            )],
        ),
    ]);
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new(
        "parent_mismatch",
        "parent-model",
        "parent system",
        "delegate",
    );
    task.sub_agents.insert(
        "researcher".to_string(),
        SubAgentConfig::new("child-model", "research profile"),
    );

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let sub_task_result = result
        .cycles
        .iter()
        .flat_map(|cycle| &cycle.tool_results)
        .find(|tool_result| tool_result.tool_call_id == "parent_sub_call")
        .expect("sub-task tool result");
    assert_eq!(sub_task_result.status, vv_agent::ToolResultStatus::Error);
    assert_eq!(
        sub_task_result.error_code.as_deref(),
        Some("sub_task_failed")
    );
    let payload: serde_json::Value =
        serde_json::from_str(&sub_task_result.content).expect("sub-task payload");
    assert_eq!(payload["status"], "failed");
    assert!(payload["error"]
        .as_str()
        .is_some_and(|error| error.contains("requires runtime settings_file")));
}

#[test]
fn runtime_adds_generated_prompt_sections_to_sub_agent_metadata() {
    let mut sub_task_args = BTreeMap::new();
    sub_task_args.insert("agent_id".to_string(), json!("researcher"));
    sub_task_args.insert(
        "task_description".to_string(),
        json!("Inspect generated prompt sections"),
    );
    let mut parent_finish_args = BTreeMap::new();
    parent_finish_args.insert("message".to_string(), json!("parent saw prompt metadata"));

    let llm = InspectingSubAgentPromptLlmClient::new(vec![
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "parent_sub_call",
                "create_sub_task",
                sub_task_args,
            )],
        ),
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "parent_finish",
                "task_finish",
                parent_finish_args,
            )],
        ),
    ]);
    let inspector = llm.clone();
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new("parent_prompt", "demo", "parent system", "delegate");
    task.metadata.insert("language".to_string(), json!("zh-CN"));
    task.metadata.insert(
        "available_skills".to_string(),
        json!([{"name": "review-code", "description": "Review code"}]),
    );
    task.sub_agents.insert(
        "researcher".to_string(),
        SubAgentConfig::new("demo", "research profile"),
    );

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let metadata = inspector
        .child_system_metadata()
        .expect("child system metadata");
    let sections = metadata["system_prompt_sections"]
        .as_array()
        .expect("system prompt sections");
    assert!(sections
        .iter()
        .any(|section| section["id"] == "agent_definition"));
    assert!(sections.iter().any(|section| section["id"] == "tools"));
}

#[test]
fn runtime_preserves_sub_agent_prompt_cache_metadata() {
    let mut sub_task_args = BTreeMap::new();
    sub_task_args.insert("agent_id".to_string(), json!("researcher"));
    sub_task_args.insert(
        "task_description".to_string(),
        json!("Inspect configured prompt sections"),
    );
    let mut parent_finish_args = BTreeMap::new();
    parent_finish_args.insert(
        "message".to_string(),
        json!("parent saw configured prompt metadata"),
    );

    let llm = InspectingSubAgentPromptLlmClient::new(vec![
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "parent_sub_call",
                "create_sub_task",
                sub_task_args,
            )],
        ),
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "parent_finish",
                "task_finish",
                parent_finish_args,
            )],
        ),
    ]);
    let inspector = llm.clone();
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new(
        "parent_prompt_configured",
        "demo",
        "parent system",
        "delegate",
    );
    let mut sub_agent = SubAgentConfig::new("demo", "research profile");
    sub_agent
        .metadata
        .insert("anthropic_prompt_cache_enabled".to_string(), json!(true));
    sub_agent.metadata.insert(
        "system_prompt_sections".to_string(),
        json!([
            {"id": "core_identity", "text": "stable section", "stable": true}
        ]),
    );
    task.sub_agents.insert("researcher".to_string(), sub_agent);

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let metadata = inspector
        .child_system_metadata()
        .expect("child system metadata");
    assert_eq!(metadata["anthropic_prompt_cache_enabled"], json!(true));
    let sections = metadata["system_prompt_sections"]
        .as_array()
        .expect("system prompt sections");
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0]["id"], json!("core_identity"));
}

#[test]
fn runtime_sub_agent_identity_metadata_cannot_be_overridden_by_request() {
    let mut sub_task_args = BTreeMap::new();
    sub_task_args.insert("agent_id".to_string(), json!("researcher"));
    sub_task_args.insert(
        "task_description".to_string(),
        json!("Inspect isolated metadata"),
    );
    let mut parent_finish_args = BTreeMap::new();
    parent_finish_args.insert("message".to_string(), json!("parent saw isolated metadata"));

    let llm = InspectingSubAgentPromptLlmClient::new(vec![
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "parent_sub_call",
                "create_sub_task",
                sub_task_args,
            )],
        ),
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::new(
                "parent_finish",
                "task_finish",
                parent_finish_args,
            )],
        ),
    ]);
    let inspector = llm.clone();
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new("parent_identity", "demo", "parent system", "delegate");
    let mut sub_agent = SubAgentConfig::new("demo", "research profile");
    sub_agent
        .metadata
        .insert("task_id".to_string(), json!("sub-agent-task-override"));
    sub_agent.metadata.insert(
        "session_id".to_string(),
        json!("sub-agent-session-override"),
    );
    sub_agent.metadata.insert(
        "browser_scope_key".to_string(),
        json!("sub-agent-browser-override"),
    );
    for key in [
        "is_sub_task",
        "parent_task_id",
        "sub_agent_name",
        "session_memory_enabled",
        "workspace",
    ] {
        sub_agent
            .metadata
            .insert(key.to_string(), json!(format!("sub-agent-override-{key}")));
    }
    task.sub_agents.insert("researcher".to_string(), sub_agent);

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let metadata = inspector
        .child_system_metadata()
        .expect("child system metadata");
    let task_id = metadata["task_id"].as_str().expect("task id");
    let session_id = metadata["session_id"].as_str().expect("session id");
    assert_ne!(task_id, "sub-agent-task-override");
    assert_ne!(session_id, "sub-agent-session-override");
    assert_eq!(session_id, task_id);
    assert_eq!(metadata["browser_scope_key"], metadata["session_id"]);
    assert_eq!(metadata["is_sub_task"], true);
    assert_eq!(metadata["parent_task_id"], "parent_identity");
    assert_eq!(metadata["sub_agent_name"], "researcher");
    assert_eq!(metadata["session_memory_enabled"], false);
    assert_ne!(metadata["workspace"], "sub-agent-override-workspace");
}
#[derive(Clone)]
struct InspectingSubAgentPromptLlmClient {
    responses: Arc<Mutex<VecDeque<LLMResponse>>>,
    child_system_metadata: Arc<Mutex<Option<BTreeMap<String, serde_json::Value>>>>,
}

impl InspectingSubAgentPromptLlmClient {
    fn new(responses: Vec<LLMResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::from(responses))),
            child_system_metadata: Arc::new(Mutex::new(None)),
        }
    }

    fn child_system_metadata(&self) -> Option<BTreeMap<String, serde_json::Value>> {
        self.child_system_metadata
            .lock()
            .expect("child metadata poisoned")
            .clone()
    }
}

impl LlmClient for InspectingSubAgentPromptLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let is_child_request = request
            .messages
            .first()
            .is_some_and(|message| message.content.contains("research profile"));
        if is_child_request {
            let metadata = request
                .messages
                .first()
                .map(|message| message.metadata.clone())
                .unwrap_or_default();
            *self
                .child_system_metadata
                .lock()
                .expect("child metadata poisoned") = Some(metadata);
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::new(
                    "child_prompt_finish",
                    "task_finish",
                    BTreeMap::from([("message".to_string(), json!("child saw prompt"))]),
                )],
            ));
        }

        self.responses
            .lock()
            .map_err(|_| LlmError::Request("inspector poisoned".to_string()))?
            .pop_front()
            .ok_or(LlmError::ScriptExhausted)
    }
}
#[derive(Clone, Default)]
struct StreamingSubAgentLlmClient {
    calls_seen: Arc<Mutex<usize>>,
}

impl LlmClient for StreamingSubAgentLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        self.complete_with_stream(request, None)
    }

    fn complete_with_stream(
        &self,
        _request: LlmRequest,
        stream_callback: Option<LlmStreamCallback>,
    ) -> Result<LLMResponse, LlmError> {
        let mut calls_seen = self
            .calls_seen
            .lock()
            .map_err(|_| LlmError::Request("call counter poisoned".to_string()))?;
        *calls_seen += 1;
        match *calls_seen {
            1 => Ok(LLMResponse::with_tool_calls(
                "delegate",
                vec![ToolCall::new(
                    "parent_sub_call",
                    "create_sub_task",
                    BTreeMap::from([
                        ("agent_id".to_string(), json!("researcher")),
                        ("task_description".to_string(), json!("Collect core facts")),
                    ]),
                )],
            )),
            2 => {
                if let Some(callback) = stream_callback {
                    callback(&BTreeMap::from([
                        ("event".to_string(), json!("assistant_delta")),
                        ("content_delta".to_string(), json!("checking")),
                        ("task_id".to_string(), json!("spoofed-task")),
                        ("session_id".to_string(), json!("spoofed-session")),
                        ("sub_agent_name".to_string(), json!("spoofed-agent")),
                    ]));
                    callback(&BTreeMap::from([
                        ("event".to_string(), json!("tool_call_started")),
                        ("tool_call_id".to_string(), json!("sub_tool_1")),
                        ("tool_call_index".to_string(), json!(0)),
                        ("function_name".to_string(), json!("bash")),
                        ("arguments_chars".to_string(), json!(0)),
                        ("estimated_tokens".to_string(), json!(0)),
                    ]));
                    callback(&BTreeMap::from([
                        ("event".to_string(), json!("tool_call_progress")),
                        ("tool_call_id".to_string(), json!("sub_tool_1")),
                        ("tool_call_index".to_string(), json!(0)),
                        ("function_name".to_string(), json!("bash")),
                        ("arguments_chars".to_string(), json!(48)),
                        ("estimated_tokens".to_string(), json!(12)),
                    ]));
                }
                Ok(LLMResponse::with_tool_calls(
                    "sub finish",
                    vec![ToolCall::new(
                        "sub_finish",
                        "task_finish",
                        BTreeMap::from([("message".to_string(), json!("sub done"))]),
                    )],
                ))
            }
            _ => Ok(LLMResponse::with_tool_calls(
                "parent finish",
                vec![ToolCall::new(
                    "parent_finish",
                    "task_finish",
                    BTreeMap::from([("message".to_string(), json!("parent done"))]),
                )],
            )),
        }
    }
}
