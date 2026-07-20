use super::*;

#[test]
fn runtime_hooks_can_patch_llm_request_and_tool_result_flow() {
    let hook = Arc::new(InspectingRuntimeHook::default());
    let llm = HookInspectingLlmClient::default();
    let inspector = llm.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.hooks.push(hook.clone());

    let result = runtime
        .run(AgentTask::new("hook_task", "demo", "system", "original"))
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.final_answer.as_deref(),
        Some("final answer patched by after_tool_call")
    );
    assert!(inspector.saw_hooked_message());
    assert_eq!(inspector.tool_schema_counts(), vec![0]);
    assert_eq!(
        hook.events(),
        vec![
            "before_llm",
            "after_llm",
            "before_tool_call",
            "after_tool_call"
        ]
    );
    assert_eq!(result.cycles[0].tool_results[0].tool_call_id, "hook_finish");
    assert!(result
        .messages
        .last()
        .expect("tool message")
        .content
        .contains("final answer patched by after_tool_call"));
}

#[test]
fn runtime_hooks_normalize_pending_tool_call_ids() {
    let hook = Arc::new(PendingToolCallIdHook);
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "finish through pending hook",
        vec![ToolCall::new(
            "pending_hook_finish",
            "task_finish",
            BTreeMap::from([("message".to_string(), json!("original"))]),
        )],
    )]);
    let mut runtime = AgentRuntime::new(llm);
    runtime.hooks.push(hook);

    let result = runtime
        .run(AgentTask::new(
            "pending_hook_task",
            "demo",
            "system",
            "finish",
        ))
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.final_answer.as_deref(),
        Some("finished by pending hook")
    );
    assert_eq!(
        result.cycles[0].tool_results[0].tool_call_id,
        "pending_hook_finish"
    );
    assert_eq!(
        result.messages.last().unwrap().tool_call_id.as_deref(),
        Some("pending_hook_finish")
    );
}

#[test]
fn before_tool_call_patch_accepts_direct_result_and_call_conversions() {
    let result_hook = Arc::new(DirectResultBeforeToolHook);
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "finish through direct hook result",
        vec![ToolCall::new(
            "direct_result_finish",
            "task_finish",
            BTreeMap::from([("message".to_string(), json!("original"))]),
        )],
    )]);
    let mut runtime = AgentRuntime::new(llm);
    runtime.hooks.push(result_hook);

    let result = runtime
        .run(AgentTask::new(
            "direct_result_hook_task",
            "demo",
            "system",
            "go",
        ))
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.final_answer.as_deref(),
        Some("finished by direct result hook")
    );

    let call_hook = Arc::new(PatchCallBeforeToolHook);
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "finish through patched hook call",
        vec![ToolCall::new(
            "patch_call_finish",
            "task_finish",
            BTreeMap::from([("message".to_string(), json!("original"))]),
        )],
    )]);
    let mut runtime = AgentRuntime::new(llm);
    runtime.hooks.push(call_hook);

    let result = runtime
        .run(AgentTask::new(
            "patch_call_hook_task",
            "demo",
            "system",
            "go",
        ))
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.final_answer.as_deref(),
        Some("finished by patched call hook")
    );
}

#[test]
fn runtime_short_circuit_tool_result_keeps_original_tool_call_id_after_call_patch() {
    let hook = Arc::new(PatchedCallAndBlankFinishHook);
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "finish through patched short circuit",
        vec![ToolCall::new(
            "runtime_original_call",
            "task_finish",
            BTreeMap::from([("message".to_string(), json!("original"))]),
        )],
    )]);
    let mut runtime = AgentRuntime::new(llm);
    runtime.hooks.push(hook);

    let result = runtime
        .run(AgentTask::new(
            "patched_short_circuit_task",
            "demo",
            "system",
            "go",
        ))
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.final_answer.as_deref(),
        Some("finished by patched short circuit")
    );
    assert_eq!(
        result.cycles[0].tool_results[0].tool_call_id,
        "runtime_original_call"
    );
    assert!(result.messages.iter().any(|message| {
        message.tool_call_id.as_deref() == Some("runtime_original_call")
            && message.content.contains("patched short circuit")
    }));
}

#[test]
fn runtime_emits_lifecycle_log_events() {
    let mut finish_args = BTreeMap::new();
    finish_args.insert("message".to_string(), json!("logged finish"));
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "assistant log",
        vec![ToolCall::new("log_finish", "task_finish", finish_args)],
    )]);
    let events = Arc::new(Mutex::new(Vec::<(
        String,
        BTreeMap<String, serde_json::Value>,
    )>::new()));
    let sink = events.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.log_handler = Some(Arc::new(Mutex::new(Box::new(
        move |event: &str, payload: &BTreeMap<String, serde_json::Value>| {
            sink.lock()
                .expect("events poisoned")
                .push((event.to_string(), payload.clone()));
        },
    ))));

    let result = runtime
        .run(AgentTask::new("log_task", "demo", "system", "finish"))
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let events = events.lock().expect("events poisoned").clone();
    let event_names = events
        .iter()
        .map(|(event, _)| event.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        event_names,
        vec![
            "run_started",
            "agent_started",
            "cycle_started",
            "llm_started",
            "cycle_llm_response",
            "tool_call_planned",
            "tool_call_started",
            "tool_call_completed",
            "tool_result",
            "run_completed"
        ]
    );
    assert_eq!(events[0].1["task_id"], "log_task");
    assert_eq!(events[0].1["model"], "demo");
    assert_eq!(events[4].1["assistant_message"], "assistant log");
    assert_eq!(events[4].1["tool_call_count"], 1);
    assert_eq!(events[5].1["tool_name"], "task_finish");
    assert_eq!(events[5].1["tool_call_id"], "log_finish");
    assert_eq!(events[6].1["tool_name"], "task_finish");
    assert_eq!(events[6].1["tool_call_id"], "log_finish");
    assert_eq!(events[7].1["tool_name"], "task_finish");
    assert_eq!(events[7].1["tool_call_id"], "log_finish");
    assert_eq!(events[7].1["directive"], "finish");
    assert_eq!(events[7].1["execution_started"], true);
    assert_eq!(events[8].1["tool_name"], "task_finish");
    assert_eq!(events[8].1["tool_call_id"], "log_finish");
    assert_eq!(events[8].1["directive"], "finish");
    assert_eq!(events[9].1["final_answer"], "logged finish");
}

#[test]
fn runtime_log_events_include_agent_previews() {
    let assistant_text = "assistant preview text ".repeat(4);
    let final_text = "final answer preview text ".repeat(4);
    let mut finish_args = BTreeMap::new();
    finish_args.insert("message".to_string(), json!(final_text.clone()));
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        assistant_text.clone(),
        vec![ToolCall::new("preview_finish", "task_finish", finish_args)],
    )]);
    let events = Arc::new(Mutex::new(Vec::<(
        String,
        BTreeMap<String, serde_json::Value>,
    )>::new()));
    let sink = events.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.log_preview_chars = Some(10);
    runtime.log_handler = Some(Arc::new(Mutex::new(Box::new(
        move |event: &str, payload: &BTreeMap<String, serde_json::Value>| {
            sink.lock()
                .expect("events poisoned")
                .push((event.to_string(), payload.clone()));
        },
    ))));

    let result = runtime
        .run(AgentTask::new(
            "preview_task",
            "demo",
            "system",
            "finish with previews",
        ))
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let events = events.lock().expect("events poisoned").clone();
    let cycle_event = events
        .iter()
        .find(|(event, _)| event == "cycle_llm_response")
        .expect("cycle llm response");
    let tool_event = events
        .iter()
        .find(|(event, _)| event == "tool_result")
        .expect("tool result");
    let completed_event = events
        .iter()
        .find(|(event, _)| event == "run_completed")
        .expect("run completed");
    assert_eq!(cycle_event.1["assistant_message"], assistant_text);
    assert_eq!(
        cycle_event.1["assistant_preview"],
        preview_text_for_test(&assistant_text, Some(10))
    );
    assert_eq!(
        tool_event.1["content_preview"],
        preview_text_for_test(tool_event.1["content"].as_str().expect("content"), Some(10))
    );
    assert_eq!(
        completed_event.1["final_answer"],
        preview_text_for_test(&final_text, Some(10))
    );
}

#[test]
fn runtime_tool_result_event_keeps_full_content_by_default() {
    let long_title = "x".repeat(500);
    let todo_args = BTreeMap::from([(
        "todos".to_string(),
        json!([{"title": long_title, "status": "completed", "priority": "medium"}]),
    )]);
    let finish_args = BTreeMap::from([("message".to_string(), json!("ok"))]);
    let llm = ScriptedLlmClient::new(vec![
        LLMResponse::with_tool_calls(
            "write todo",
            vec![ToolCall::new("todo_long", "todo_write", todo_args)],
        ),
        LLMResponse::with_tool_calls(
            "done",
            vec![ToolCall::new("finish_long", "task_finish", finish_args)],
        ),
    ]);
    let events = Arc::new(Mutex::new(Vec::<(
        String,
        BTreeMap<String, serde_json::Value>,
    )>::new()));
    let sink = events.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.log_handler = Some(Arc::new(Mutex::new(Box::new(
        move |event: &str, payload: &BTreeMap<String, serde_json::Value>| {
            sink.lock()
                .expect("events poisoned")
                .push((event.to_string(), payload.clone()));
        },
    ))));

    let mut task = AgentTask::new("task_long_tool_result", "demo", "system", "go");
    task.max_cycles = 4;
    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    let events = events.lock().expect("events poisoned").clone();
    let tool_event = events
        .iter()
        .find(|(event, _)| event == "tool_result")
        .expect("tool result");
    let full_content = tool_event.1["content"].as_str().expect("content");
    assert!(full_content.contains(&long_title));
    assert!(full_content.len() > 220);
    assert_eq!(
        tool_event.1["content_preview"].as_str().expect("preview"),
        full_content
    );
}

#[test]
fn runtime_emits_run_max_cycles_log_with_final_answer() {
    let llm = ScriptedLlmClient::new(vec![LLMResponse::new("step 1"), LLMResponse::new("step 2")]);
    let events = Arc::new(Mutex::new(Vec::<(
        String,
        BTreeMap<String, serde_json::Value>,
    )>::new()));
    let sink = events.clone();
    let mut runtime = AgentRuntime::new(llm);
    runtime.log_handler = Some(Arc::new(Mutex::new(Box::new(
        move |event: &str, payload: &BTreeMap<String, serde_json::Value>| {
            sink.lock()
                .expect("events poisoned")
                .push((event.to_string(), payload.clone()));
        },
    ))));
    let mut task = AgentTask::new("max_cycles_log", "demo", "system", "keep going");
    task.max_cycles = 2;

    let result = runtime.run(task).expect("run");

    assert_eq!(result.status, AgentStatus::MaxCycles);
    let events = events.lock().expect("events poisoned").clone();
    let max_cycles = events
        .iter()
        .find(|(event, _)| event == "run_max_cycles")
        .expect("run max cycles event");
    assert_eq!(max_cycles.1["cycle"], json!(2));
    assert_eq!(
        max_cycles.1["final_answer"],
        json!("Reached max cycles without finish signal.")
    );
}

#[test]
fn runtime_controls_can_inject_messages_before_each_cycle() {
    let mut finish_args = BTreeMap::new();
    finish_args.insert("message".to_string(), json!("saw injected message"));
    let llm = ScriptedLlmClient::new(vec![LLMResponse::with_tool_calls(
        "finish",
        vec![ToolCall::new("finish_injected", "task_finish", finish_args)],
    )]);
    let runtime = AgentRuntime::new(llm);

    let result = runtime
        .run_with_controls(
            AgentTask::new("before_cycle_task", "demo", "system", "start"),
            RuntimeRunControls {
                before_cycle_messages: Some(Arc::new(|cycle_index, messages, shared_state| {
                    assert_eq!(cycle_index, 1);
                    assert_eq!(messages.len(), 2);
                    assert!(shared_state.contains_key("todo_list"));
                    vec![Message::user("injected before cycle")]
                })),
                ..RuntimeRunControls::default()
            },
        )
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert!(result
        .messages
        .iter()
        .any(|message| message.content == "injected before cycle"));
}

#[test]
fn runtime_interruption_provider_skips_remaining_tools() {
    let mut registry = vv_agent::tools::build_default_registry();
    registry
        .register_tool(
            "_demo_noop",
            "noop",
            Arc::new(|_context, _arguments| ToolExecutionResult::success("", "{}")),
        )
        .expect("register noop");

    let mut finish_args = BTreeMap::new();
    finish_args.insert("message".to_string(), json!("done"));
    let llm = ScriptedLlmClient::new(vec![
        LLMResponse::with_tool_calls(
            "two tools",
            vec![
                ToolCall::new("t1", "_demo_noop", BTreeMap::new()),
                ToolCall::new("t2", "_demo_noop", BTreeMap::new()),
            ],
        ),
        LLMResponse::with_tool_calls(
            "finish",
            vec![ToolCall::new(
                "finish_after_steer",
                "task_finish",
                finish_args,
            )],
        ),
    ]);
    let runtime = AgentRuntime::new(llm).with_tool_registry(registry);
    let used = Arc::new(Mutex::new(false));
    let provider_used = used.clone();
    let events = Arc::new(Mutex::new(Vec::<(
        String,
        BTreeMap<String, serde_json::Value>,
    )>::new()));
    let sink = events.clone();

    let mut task = AgentTask::new("steer_skip", "demo", "system", "go");
    task.max_cycles = 4;
    task.extra_tool_names = vec!["_demo_noop".to_string()];

    let result = runtime
        .run_with_controls(
            task,
            RuntimeRunControls {
                interruption_messages: Some(Arc::new(move || {
                    let mut used = provider_used.lock().expect("provider flag");
                    if *used {
                        Vec::new()
                    } else {
                        *used = true;
                        vec![Message::user("STEER_NOW")]
                    }
                })),
                log_handler: Some(Arc::new(move |event, payload| {
                    sink.lock()
                        .expect("events")
                        .push((event.to_string(), payload.clone()));
                })),
                ..RuntimeRunControls::default()
            },
        )
        .expect("run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(
        result.cycles[0].tool_results[1].error_code.as_deref(),
        Some("skipped_due_to_steering")
    );
    assert!(result
        .messages
        .iter()
        .any(|message| message.content == "STEER_NOW"));
    let events = events.lock().expect("events").clone();
    assert!(events.iter().any(|(event, _)| event == "run_steered"));
}
struct PendingToolCallIdHook;

impl RuntimeHook for PendingToolCallIdHook {
    fn before_tool_call(
        &self,
        event: vv_agent::BeforeToolCallEvent<'_>,
    ) -> Option<BeforeToolCallPatch> {
        assert_eq!(event.call.id, "pending_hook_finish");
        let mut result = ToolExecutionResult::success(
            "pending",
            json!({"message": "finished by pending hook"}).to_string(),
        );
        result.directive = ToolDirective::Finish;
        result.metadata.insert(
            "final_message".to_string(),
            json!("finished by pending hook"),
        );
        Some(BeforeToolCallPatch {
            call: None,
            result: Some(result),
        })
    }
}

struct DirectResultBeforeToolHook;

impl RuntimeHook for DirectResultBeforeToolHook {
    fn before_tool_call(
        &self,
        event: vv_agent::BeforeToolCallEvent<'_>,
    ) -> Option<BeforeToolCallPatch> {
        assert_eq!(event.call.id, "direct_result_finish");
        let mut result = ToolExecutionResult::success(
            event.call.id.clone(),
            json!({"message": "finished by direct result hook"}).to_string(),
        );
        result.directive = ToolDirective::Finish;
        result.metadata.insert(
            "final_message".to_string(),
            json!("finished by direct result hook"),
        );
        Some(result.into())
    }
}

struct PatchCallBeforeToolHook;

impl RuntimeHook for PatchCallBeforeToolHook {
    fn before_tool_call(
        &self,
        event: vv_agent::BeforeToolCallEvent<'_>,
    ) -> Option<BeforeToolCallPatch> {
        assert_eq!(event.call.id, "patch_call_finish");
        let mut patched = event.call.clone();
        patched.arguments.insert(
            "message".to_string(),
            json!("finished by patched call hook"),
        );
        Some(patched.into())
    }
}

struct PatchedCallAndBlankFinishHook;

impl RuntimeHook for PatchedCallAndBlankFinishHook {
    fn before_tool_call(
        &self,
        event: vv_agent::BeforeToolCallEvent<'_>,
    ) -> Option<BeforeToolCallPatch> {
        let mut patched = event.call.clone();
        patched.id = "runtime_patched_call".to_string();
        let mut result = ToolExecutionResult::success(
            "",
            json!({"message": "finished by patched short circuit"}).to_string(),
        );
        result.directive = ToolDirective::Finish;
        result.metadata.insert(
            "final_message".to_string(),
            json!("finished by patched short circuit"),
        );
        Some(BeforeToolCallPatch {
            call: Some(patched),
            result: Some(result),
        })
    }
}

#[derive(Default)]
struct InspectingRuntimeHook {
    events: Mutex<Vec<&'static str>>,
}

impl InspectingRuntimeHook {
    fn events(&self) -> Vec<&'static str> {
        self.events.lock().expect("events poisoned").clone()
    }
}

impl RuntimeHook for InspectingRuntimeHook {
    fn before_llm(&self, event: vv_agent::BeforeLlmEvent<'_>) -> Option<BeforeLlmPatch> {
        assert_eq!(event.cycle_index, 1);
        assert_eq!(event.task.task_id, "hook_task");
        assert!(event.shared_state.contains_key("todo_list"));
        self.events
            .lock()
            .expect("events poisoned")
            .push("before_llm");
        Some(BeforeLlmPatch {
            messages: Some(vec![Message::user("hooked user request")]),
            tool_schemas: Some(Vec::new()),
        })
    }

    fn after_llm(&self, event: vv_agent::AfterLlmEvent<'_>) -> Option<LLMResponse> {
        assert_eq!(event.messages[0].content, "hooked user request");
        assert!(event.tool_schemas.is_empty());
        self.events
            .lock()
            .expect("events poisoned")
            .push("after_llm");
        Some(event.response.clone())
    }

    fn before_tool_call(
        &self,
        event: vv_agent::BeforeToolCallEvent<'_>,
    ) -> Option<BeforeToolCallPatch> {
        assert_eq!(event.call.name, "task_finish");
        assert_eq!(event.context.cycle_index, 1);
        self.events
            .lock()
            .expect("events poisoned")
            .push("before_tool_call");
        Some(BeforeToolCallPatch {
            call: None,
            result: Some(ToolExecutionResult::success(
                event.call.id.clone(),
                json!({"message": "short-circuited by hook"}).to_string(),
            )),
        })
    }

    fn after_tool_call(
        &self,
        event: vv_agent::AfterToolCallEvent<'_>,
    ) -> Option<ToolExecutionResult> {
        assert_eq!(event.call.id, "hook_finish");
        assert!(event.result.content.contains("short-circuited"));
        self.events
            .lock()
            .expect("events poisoned")
            .push("after_tool_call");
        let mut result = event.result.clone();
        result.directive = ToolDirective::Finish;
        result.content = json!({"message": "final answer patched by after_tool_call"}).to_string();
        result.metadata.insert(
            "final_message".to_string(),
            json!("final answer patched by after_tool_call"),
        );
        Some(result)
    }
}

#[derive(Clone, Default)]
struct HookInspectingLlmClient {
    saw_hooked_message: Arc<Mutex<bool>>,
    tool_schema_counts: Arc<Mutex<Vec<usize>>>,
}

impl HookInspectingLlmClient {
    fn saw_hooked_message(&self) -> bool {
        *self.saw_hooked_message.lock().expect("flag poisoned")
    }

    fn tool_schema_counts(&self) -> Vec<usize> {
        self.tool_schema_counts
            .lock()
            .expect("schema counts poisoned")
            .clone()
    }
}

impl LlmClient for HookInspectingLlmClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        if request
            .messages
            .iter()
            .any(|message| message.content == "hooked user request")
        {
            *self.saw_hooked_message.lock().expect("flag poisoned") = true;
        }
        self.tool_schema_counts
            .lock()
            .expect("schema counts poisoned")
            .push(request.tools.len());
        Ok(LLMResponse::with_tool_calls(
            "finish through hook",
            vec![ToolCall::new(
                "hook_finish",
                "task_finish",
                BTreeMap::from([("message".to_string(), json!("original finish"))]),
            )],
        ))
    }
}
