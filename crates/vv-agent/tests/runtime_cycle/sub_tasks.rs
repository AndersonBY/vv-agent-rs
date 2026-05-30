use super::*;

#[test]
fn runtime_seeds_skill_state_from_task_metadata() {
    let mut finish_args = BTreeMap::new();
    finish_args.insert("message".to_string(), json!("done"));
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "finish",
        vec![ToolCall::new(
            "finish_skill_state",
            "task_finish",
            finish_args,
        )],
    )]);
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new("skill_state", "demo", "system", "finish");
    task.metadata.insert(
        "available_skills".to_string(),
        json!([{"name": "demo", "description": "Demo skill"}]),
    );
    task.metadata
        .insert("active_skills".to_string(), json!(["already-active"]));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.shared_state["available_skills"],
        json!([{"name": "demo", "description": "Demo skill"}])
    );
    assert_eq!(
        result.shared_state["active_skills"],
        json!(["already-active"])
    );
}

#[test]
fn runtime_keeps_initial_skill_state_over_task_metadata() {
    let mut finish_args = BTreeMap::new();
    finish_args.insert("message".to_string(), json!("done"));
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "finish",
        vec![ToolCall::new(
            "finish_initial_skill_state",
            "task_finish",
            finish_args,
        )],
    )]);
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new("initial_skill_state", "demo", "system", "finish");
    task.metadata.insert(
        "available_skills".to_string(),
        json!([{"name": "metadata-skill", "description": "Metadata skill"}]),
    );
    task.metadata
        .insert("active_skills".to_string(), json!(["metadata-active"]));
    task.initial_shared_state.insert(
        "available_skills".to_string(),
        json!([{"name": "state-skill", "description": "State skill"}]),
    );
    task.initial_shared_state
        .insert("active_skills".to_string(), json!(["state-active"]));

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.shared_state["available_skills"],
        json!([{"name": "state-skill", "description": "State skill"}])
    );
    assert_eq!(
        result.shared_state["active_skills"],
        json!(["state-active"])
    );
}

#[test]
fn runtime_can_poll_async_configured_sub_agent_status() {
    let mut sub_task_args = BTreeMap::new();
    sub_task_args.insert("agent_id".to_string(), json!("researcher"));
    sub_task_args.insert(
        "task_description".to_string(),
        json!("Collect async task facts"),
    );
    sub_task_args.insert("wait_for_completion".to_string(), json!(false));
    let llm = InspectingSubTaskStatusLlmClient::new(vec![LLMResponse::with_tool_calls(
        "",
        vec![ToolCall::new(
            "parent_async_sub_call",
            "create_sub_task",
            sub_task_args,
        )],
    )]);
    let inspector = llm.clone();
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new("parent_async", "demo", "parent system", "delegate async");
    task.max_cycles = 50;
    task.sub_agents.insert(
        "researcher".to_string(),
        SubAgentConfig::new("demo", "research profile"),
    );

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.final_answer.as_deref(),
        Some("parent saw async child result")
    );
    assert!(inspector.status_payloads().iter().any(|payload| {
        payload["tasks"][0]["status"] == "completed"
            && payload["tasks"][0]["final_answer"] == "async child complete"
    }));
}

#[test]
fn runtime_can_continue_completed_async_sub_agent_session() {
    let mut sub_task_args = BTreeMap::new();
    sub_task_args.insert("agent_id".to_string(), json!("researcher"));
    sub_task_args.insert(
        "task_description".to_string(),
        json!("Collect async task facts"),
    );
    sub_task_args.insert("wait_for_completion".to_string(), json!(false));
    let llm = InspectingSubTaskContinuationLlmClient::new(vec![LLMResponse::with_tool_calls(
        "",
        vec![ToolCall::new(
            "parent_async_sub_call",
            "create_sub_task",
            sub_task_args,
        )],
    )]);
    let inspector = llm.clone();
    let runtime = AgentRuntime::new(llm);
    let mut task = AgentTask::new(
        "parent_async_continue",
        "demo",
        "parent system",
        "delegate async",
    );
    task.max_cycles = 50;
    task.sub_agents.insert(
        "researcher".to_string(),
        SubAgentConfig::new("demo", "research profile"),
    );

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.final_answer.as_deref(),
        Some("parent saw followed-up child result")
    );
    assert!(inspector.status_payloads().iter().any(|payload| {
        payload["interaction"]["action"] == "continued"
            && payload["tasks"][0]["final_answer"] == "follow-up child complete"
    }));
}
#[derive(Clone)]
struct InspectingSubTaskStatusLlmClient {
    responses: Arc<Mutex<VecDeque<LLMResponse>>>,
    status_payloads: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl InspectingSubTaskStatusLlmClient {
    fn new(responses: Vec<LLMResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::from(responses))),
            status_payloads: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn status_payloads(&self) -> Vec<serde_json::Value> {
        self.status_payloads
            .lock()
            .expect("status payloads poisoned")
            .clone()
    }
}

impl LlmClient for InspectingSubTaskStatusLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let is_child_request = request
            .messages
            .first()
            .is_some_and(|message| message.content.contains("research profile"));
        if is_child_request {
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::new(
                    "child_async_finish",
                    "task_finish",
                    BTreeMap::from([("message".to_string(), json!("async child complete"))]),
                )],
            ));
        }
        if !is_child_request {
            let latest_async_task_id = request
                .messages
                .iter()
                .rev()
                .filter_map(|message| {
                    if message.role != vv_agent::MessageRole::Tool
                        || message.tool_call_id.as_deref() != Some("parent_async_sub_call")
                    {
                        return None;
                    }
                    let payload: serde_json::Value = serde_json::from_str(&message.content).ok()?;
                    payload
                        .get("task_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
                .next();
            if let Some(task_id) = latest_async_task_id {
                if !request.messages.iter().any(|message| {
                    message.role == vv_agent::MessageRole::Tool
                        && message.tool_call_id.as_deref() == Some("parent_async_status")
                }) {
                    return Ok(LLMResponse::with_tool_calls(
                        "",
                        vec![ToolCall::new(
                            "parent_async_status",
                            "sub_task_status",
                            BTreeMap::from([
                                ("task_ids".to_string(), json!([task_id])),
                                ("detail_level".to_string(), json!("snapshot")),
                            ]),
                        )],
                    ));
                }
            }
        }

        let mut latest_status_payload = None;
        for message in &request.messages {
            if message.role == vv_agent::MessageRole::Tool
                && message.tool_call_id.as_deref() == Some("parent_async_status")
            {
                if let Ok(payload) = serde_json::from_str::<serde_json::Value>(&message.content) {
                    self.status_payloads
                        .lock()
                        .expect("status payloads poisoned")
                        .push(payload.clone());
                    latest_status_payload = Some(payload);
                }
            }
        }
        if let Some(payload) = latest_status_payload {
            let completed = payload["tasks"]
                .as_array()
                .and_then(|tasks| tasks.first())
                .is_some_and(|task| task["status"] == "completed");
            if completed {
                return Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::new(
                        "parent_finish",
                        "task_finish",
                        BTreeMap::from([(
                            "message".to_string(),
                            json!("parent saw async child result"),
                        )]),
                    )],
                ));
            }
            if let Some(task_id) = payload["tasks"]
                .as_array()
                .and_then(|tasks| tasks.first())
                .and_then(|task| task["task_id"].as_str())
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
                return Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::new(
                        "parent_async_status",
                        "sub_task_status",
                        BTreeMap::from([
                            ("task_ids".to_string(), json!([task_id])),
                            ("detail_level".to_string(), json!("snapshot")),
                        ]),
                    )],
                ));
            }
        }

        self.responses
            .lock()
            .map_err(|_| LlmError::Request("inspector poisoned".to_string()))?
            .pop_front()
            .ok_or(LlmError::ScriptExhausted)
    }
}

#[derive(Clone)]
struct InspectingSubTaskContinuationLlmClient {
    responses: Arc<Mutex<VecDeque<LLMResponse>>>,
    status_payloads: Arc<Mutex<Vec<serde_json::Value>>>,
}

impl InspectingSubTaskContinuationLlmClient {
    fn new(responses: Vec<LLMResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::from(responses))),
            status_payloads: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn status_payloads(&self) -> Vec<serde_json::Value> {
        self.status_payloads
            .lock()
            .expect("status payloads poisoned")
            .clone()
    }
}

impl LlmClient for InspectingSubTaskContinuationLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        let is_child_request = request
            .messages
            .first()
            .is_some_and(|message| message.content.contains("research profile"));
        if is_child_request {
            let is_follow_up = request.messages.iter().any(|message| {
                message.role == vv_agent::MessageRole::User
                    && message.content.contains("Add appendix")
            });
            let message = if is_follow_up {
                "follow-up child complete"
            } else {
                "initial child complete"
            };
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::new(
                    if is_follow_up {
                        "child_follow_up_finish"
                    } else {
                        "child_initial_finish"
                    },
                    "task_finish",
                    BTreeMap::from([("message".to_string(), json!(message))]),
                )],
            ));
        }

        let latest_create_task_id = request
            .messages
            .iter()
            .rev()
            .filter_map(|message| {
                if message.role != vv_agent::MessageRole::Tool
                    || message.tool_call_id.as_deref() != Some("parent_async_sub_call")
                {
                    return None;
                }
                let payload: serde_json::Value = serde_json::from_str(&message.content).ok()?;
                payload
                    .get("task_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .next();

        if let Some(task_id) = latest_create_task_id {
            let mut latest_status_payload = None;
            let mut saw_continue_result = false;
            for message in &request.messages {
                if message.role != vv_agent::MessageRole::Tool {
                    continue;
                }
                if message.tool_call_id.as_deref() == Some("parent_async_status")
                    || message.tool_call_id.as_deref() == Some("parent_async_continue")
                {
                    if let Ok(payload) = serde_json::from_str::<serde_json::Value>(&message.content)
                    {
                        self.status_payloads
                            .lock()
                            .expect("status payloads poisoned")
                            .push(payload.clone());
                        if message.tool_call_id.as_deref() == Some("parent_async_continue") {
                            saw_continue_result = true;
                        }
                        latest_status_payload = Some(payload);
                    }
                }
            }

            if saw_continue_result {
                let follow_up_complete = latest_status_payload.as_ref().is_some_and(|payload| {
                    payload["tasks"][0]["status"] == "completed"
                        && payload["tasks"][0]["final_answer"] == "follow-up child complete"
                });
                return Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::new(
                        "parent_finish",
                        "task_finish",
                        BTreeMap::from([(
                            "message".to_string(),
                            json!(if follow_up_complete {
                                "parent saw followed-up child result"
                            } else {
                                "parent saw follow-up failure"
                            }),
                        )]),
                    )],
                ));
            }

            if let Some(payload) = latest_status_payload {
                let completed = payload["tasks"]
                    .as_array()
                    .and_then(|tasks| tasks.first())
                    .is_some_and(|task| task["status"] == "completed");
                if completed {
                    return Ok(LLMResponse::with_tool_calls(
                        "",
                        vec![ToolCall::new(
                            "parent_async_continue",
                            "sub_task_status",
                            BTreeMap::from([
                                ("task_ids".to_string(), json!([task_id])),
                                ("detail_level".to_string(), json!("snapshot")),
                                ("message".to_string(), json!("Add appendix")),
                                ("wait_for_response".to_string(), json!(true)),
                            ]),
                        )],
                    ));
                }
            }

            std::thread::sleep(std::time::Duration::from_millis(10));
            return Ok(LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::new(
                    "parent_async_status",
                    "sub_task_status",
                    BTreeMap::from([
                        ("task_ids".to_string(), json!([task_id])),
                        ("detail_level".to_string(), json!("snapshot")),
                    ]),
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
