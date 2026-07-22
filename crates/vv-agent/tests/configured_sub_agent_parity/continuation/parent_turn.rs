use super::*;

type LifecycleEvents = Arc<Mutex<Vec<(String, BTreeMap<String, Value>)>>>;

#[derive(Clone)]
struct CurrentTurnContinuationClient {
    child_task_id: Arc<Mutex<Option<String>>>,
    child_metadata: Arc<Mutex<BTreeMap<String, Vec<Value>>>>,
    turn_c_started: std::sync::mpsc::Sender<()>,
    turn_c_started_once: Arc<AtomicBool>,
    turn_c_release: Arc<(Mutex<bool>, Condvar)>,
}

impl CurrentTurnContinuationClient {
    fn request_turn(request: &LlmRequest) -> Option<&'static str> {
        for message in request.messages.iter().rev() {
            if message.role != MessageRole::User {
                continue;
            }
            for (turn, prompt) in [
                ("A", "initial child prompt"),
                ("B", "continue from parent turn B"),
                ("C", "continue from parent turn C"),
                ("D", "plain public continuation D"),
            ] {
                if message.content == prompt {
                    return Some(turn);
                }
            }
        }
        None
    }

    fn has_tool_call(request: &LlmRequest, tool_call_id: &str) -> bool {
        request
            .messages
            .iter()
            .flat_map(|message| message.tool_calls.iter())
            .any(|call| call.id == tool_call_id)
    }

    fn parent_response(&self, request: &LlmRequest, turn: &str) -> LLMResponse {
        let continuation_call_id = format!("continue-{turn}");
        if Self::has_tool_call(request, &continuation_call_id) {
            return finish_response(&format!("parent-finish-{turn}"), "parent done");
        }
        if turn == "A" {
            if Self::has_tool_call(request, "delegate-A") {
                return finish_response("parent-finish-A", "parent done");
            }
            return LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "delegate-A",
                    "create_sub_task",
                    json!({
                        "agent_id": "researcher",
                        "task_description": "initial child prompt"
                    }),
                )],
            );
        }

        let task_id = self
            .child_task_id
            .lock()
            .expect("current-turn child task id")
            .clone()
            .expect("current-turn child task id populated");
        LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                continuation_call_id,
                "sub_task_status",
                json!({
                    "task_ids": [task_id],
                    "message": format!("continue from parent turn {turn}"),
                    "wait_for_response": turn == "B"
                }),
            )],
        )
    }
}

impl LlmClient for CurrentTurnContinuationClient {
    fn complete(&self, request: LlmRequest) -> Result<LLMResponse, LlmError> {
        self.complete_with_stream(request, None)
    }

    fn complete_with_stream(
        &self,
        request: LlmRequest,
        stream_callback: Option<LlmStreamCallback>,
    ) -> Result<LLMResponse, LlmError> {
        let system_prompt = request
            .messages
            .first()
            .filter(|message| message.role == MessageRole::System)
            .map(|message| message.content.as_str())
            .unwrap_or_default();
        if let Some(turn) = system_prompt.strip_prefix("Parent prompt ") {
            return Ok(self.parent_response(&request, turn));
        }
        if system_prompt != "Child prompt" {
            return Err(LlmError::Request(format!(
                "unexpected system prompt: {system_prompt}"
            )));
        }

        let turn = Self::request_turn(&request).ok_or_else(|| {
            LlmError::Request("child request has no known turn prompt".to_string())
        })?;
        self.child_metadata
            .lock()
            .expect("current-turn child metadata")
            .entry(turn.to_string())
            .or_default()
            .push(request.metadata.clone());

        match turn {
            "A" => Ok(finish_response("child-finish-A", "initial child done")),
            "B" if !Self::has_tool_call(&request, "dangerous-B") => {
                if let Some(callback) = stream_callback {
                    callback(&BTreeMap::from([
                        ("event".to_string(), json!("assistant_delta")),
                        ("content_delta".to_string(), json!("turn B child delta")),
                    ]));
                }
                Ok(LLMResponse::with_tool_calls(
                    "",
                    vec![ToolCall::from_raw_arguments(
                        "dangerous-B",
                        "dangerous_action",
                        json!({"scope": "must be denied by turn B"}),
                    )],
                ))
            }
            "B" => Ok(finish_response("child-finish-B", "turn B child done")),
            "C" => {
                if let Some(callback) = stream_callback {
                    callback(&BTreeMap::from([
                        ("event".to_string(), json!("assistant_delta")),
                        ("content_delta".to_string(), json!("turn C child delta")),
                    ]));
                }
                if !self.turn_c_started_once.swap(true, Ordering::SeqCst) {
                    self.turn_c_started
                        .send(())
                        .expect("signal turn C child start");
                }
                let (released, wake) = &*self.turn_c_release;
                let mut released = released.lock().expect("turn C release lock");
                while !*released {
                    released = wake.wait(released).expect("turn C release wait");
                }
                Ok(finish_response(
                    "child-finish-C",
                    "turn C child should be cancelled",
                ))
            }
            "D" => Ok(finish_response("child-finish-D", "plain continuation done")),
            _ => Err(LlmError::Request(format!("unexpected child turn: {turn}"))),
        }
    }
}

fn current_turn_parent_task(turn: &str) -> AgentTask {
    let mut parent = AgentTask::new(
        format!("parent-task-{turn}"),
        "shared-model",
        format!("Parent prompt {turn}"),
        format!("parent input {turn}"),
    );
    parent.max_cycles = 3;
    parent.allow_interruption = false;
    parent.use_workspace = false;
    parent.extra_tool_names = vec!["dangerous_action".to_string()];
    let mut child = SubAgentConfig::new("shared-model", "Research");
    child.system_prompt = Some("Child prompt".to_string());
    child.max_cycles = 3;
    parent.sub_agents.insert("researcher".to_string(), child);
    parent
}

fn current_turn_controls(
    turn: &str,
    token: vv_agent::CancellationToken,
    manager: SubTaskManager,
    events: LifecycleEvents,
    session_events: Arc<Mutex<Vec<String>>>,
    streams: Arc<Mutex<Vec<BTreeMap<String, Value>>>>,
    session_event_invocations: Option<Arc<AtomicUsize>>,
) -> RuntimeRunControls {
    let event_handler: vv_agent::RunEventHandler = Arc::new(move |run_event| {
        let (name, payload) = typed_event_parts(run_event);
        if matches!(name.as_str(), "sub_run_started" | "sub_run_completed") {
            events
                .lock()
                .expect("current-turn lifecycle events")
                .push((name.to_string(), payload.clone()));
        }
        if let vv_agent::RunEventPayload::Diagnostic { code, .. } = run_event.payload() {
            if matches!(
                code.as_str(),
                "sub_agent_session_run_start" | "sub_agent_session_run_end"
            ) {
                if let Some(invocations) = &session_event_invocations {
                    invocations.fetch_add(1, Ordering::SeqCst);
                }
                session_events
                    .lock()
                    .expect("current-turn session events")
                    .push(code.clone());
            }
        }
        if name == "assistant_delta" {
            streams
                .lock()
                .expect("current-turn stream events")
                .push(payload);
        }
    });
    RuntimeRunControls {
        event_handler: Some(event_handler),
        execution_context: Some(ExecutionContext {
            cancellation_token: Some(token),
            metadata: BTreeMap::from([(
                "_vv_agent_trace_id".to_string(),
                json!(format!("trace-{turn}")),
            )]),
            ..ExecutionContext::default()
        }),
        run_context: Some(RunContext {
            run_id: format!("parent-run-{turn}"),
            agent_name: "parent".to_string(),
            ..RunContext::default()
        }),
        sub_task_manager: Some(manager),
        ..RuntimeRunControls::default()
    }
}

#[test]
fn real_sub_task_status_continuation_binds_controls_from_the_accepting_parent_turn() {
    let continuation_contract = &contract()["continuation"];
    assert_eq!(
        continuation_contract["current_parent_turn_binding"],
        json!([
            "cancellation",
            "event_sink",
            "parent_run_id",
            "parent_tool_call_id",
            "tool_policy",
            "trace_id"
        ])
    );

    let child_task_id = Arc::new(Mutex::new(None));
    let child_metadata = Arc::new(Mutex::new(BTreeMap::<String, Vec<Value>>::new()));
    let (turn_c_started_tx, turn_c_started_rx) = std::sync::mpsc::channel();
    let turn_c_release = Arc::new((Mutex::new(false), Condvar::new()));
    let client = CurrentTurnContinuationClient {
        child_task_id: child_task_id.clone(),
        child_metadata: child_metadata.clone(),
        turn_c_started: turn_c_started_tx,
        turn_c_started_once: Arc::new(AtomicBool::new(false)),
        turn_c_release: turn_c_release.clone(),
    };
    let dangerous_executions = Arc::new(AtomicUsize::new(0));
    let dangerous_executions_for_tool = dangerous_executions.clone();
    let mut registry = build_default_registry();
    registry
        .register(ToolSpec::new(
            "dangerous_action",
            "A dangerous action used to verify current-turn policy.",
            Arc::new(move |_context, _arguments| {
                dangerous_executions_for_tool.fetch_add(1, Ordering::SeqCst);
                ToolExecutionResult::success("", json!({"executed": true}).to_string())
            }),
        ))
        .expect("register dangerous action");
    let manager = SubTaskManager::default();
    let mut runtime = AgentRuntime::new(client).with_tool_registry(registry);

    let token_a = vv_agent::CancellationToken::default();
    let events_a = Arc::new(Mutex::new(Vec::new()));
    let session_events_a = Arc::new(Mutex::new(Vec::new()));
    let streams_a = Arc::new(Mutex::new(Vec::new()));
    let stale_sink_invocations_a = Arc::new(AtomicUsize::new(0));
    runtime.set_tool_policy(ToolPolicy {
        approval: ApprovalPolicy::Never,
        ..ToolPolicy::default()
    });
    let initial = runtime
        .run_with_controls(
            current_turn_parent_task("A"),
            current_turn_controls(
                "A",
                token_a.clone(),
                manager.clone(),
                events_a.clone(),
                session_events_a.clone(),
                streams_a.clone(),
                Some(stale_sink_invocations_a.clone()),
            ),
        )
        .expect("turn A creates retained child");
    let initial_payload = initial
        .cycles
        .iter()
        .flat_map(|cycle| cycle.tool_results.iter())
        .find(|result| result.tool_call_id == "delegate-A")
        .map(|result| serde_json::from_str::<Value>(&result.content).expect("turn A payload"))
        .expect("turn A child payload");
    let task_id = initial_payload["task_id"]
        .as_str()
        .expect("turn A child task id")
        .to_string();
    *child_task_id
        .lock()
        .expect("set current-turn child task id") = Some(task_id.clone());
    let events_a_after_initial = events_a.lock().expect("turn A lifecycle events").len();
    let streams_a_after_initial = streams_a.lock().expect("turn A stream events").len();
    let session_events_a_after_initial = session_events_a
        .lock()
        .expect("turn A session events")
        .len();
    let stale_sink_invocations_after_initial = stale_sink_invocations_a.load(Ordering::SeqCst);
    assert_eq!(session_events_a_after_initial, 2);
    assert_eq!(
        *session_events_a.lock().expect("turn A session events"),
        [
            "sub_agent_session_run_start".to_string(),
            "sub_agent_session_run_end".to_string(),
        ]
    );
    let initial_child_run_id = events_a
        .lock()
        .expect("turn A lifecycle events")
        .iter()
        .find(|(name, _)| name == "sub_run_started")
        .and_then(|(_, payload)| payload["run_id"].as_str())
        .expect("turn A child run id")
        .to_string();

    let token_b = vv_agent::CancellationToken::default();
    let events_b = Arc::new(Mutex::new(Vec::new()));
    let session_events_b = Arc::new(Mutex::new(Vec::new()));
    let streams_b = Arc::new(Mutex::new(Vec::new()));
    runtime.set_tool_policy(
        ToolPolicy::default().can_use_tool(|name, _arguments| name != "dangerous_action"),
    );
    let turn_b = runtime
        .run_with_controls(
            current_turn_parent_task("B"),
            current_turn_controls(
                "B",
                token_b.clone(),
                manager.clone(),
                events_b.clone(),
                session_events_b.clone(),
                streams_b.clone(),
                None,
            ),
        )
        .expect("turn B continues retained child");
    assert_eq!(turn_b.status, AgentStatus::Completed);
    assert_eq!(dangerous_executions.load(Ordering::SeqCst), 0);
    assert_eq!(
        session_events_a
            .lock()
            .expect("turn A session events")
            .len(),
        session_events_a_after_initial
    );
    assert_eq!(
        stale_sink_invocations_a.load(Ordering::SeqCst),
        stale_sink_invocations_after_initial
    );
    assert_eq!(
        *session_events_b.lock().expect("turn B session events"),
        [
            "sub_agent_session_run_start".to_string(),
            "sub_agent_session_run_end".to_string(),
        ]
    );
    assert_eq!(
        events_a.lock().expect("turn A lifecycle events").len(),
        events_a_after_initial
    );
    assert_eq!(
        streams_a.lock().expect("turn A stream events").len(),
        streams_a_after_initial
    );
    let turn_b_streams = streams_b.lock().expect("turn B stream events");
    assert_eq!(turn_b_streams.len(), 1);
    assert_eq!(turn_b_streams[0]["delta"], "turn B child delta");
    drop(turn_b_streams);
    let turn_b_events = events_b.lock().expect("turn B lifecycle events");
    assert_eq!(
        turn_b_events
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["sub_run_started", "sub_run_completed"]
    );
    for (_, payload) in turn_b_events.iter() {
        assert_eq!(payload["trace_id"], "trace-B");
        assert_eq!(payload["parent_run_id"], "parent-run-B");
        assert_eq!(payload["parent_tool_call_id"], "continue-B");
        assert_eq!(payload["task_id"], task_id);
        assert_eq!(payload["child_session_id"], initial_payload["session_id"]);
    }
    let turn_b_child_run_id = turn_b_events[0].1["run_id"]
        .as_str()
        .expect("turn B child run id")
        .to_string();
    assert_ne!(turn_b_child_run_id, initial_child_run_id);
    drop(turn_b_events);
    let after_b = manager.get(&task_id).expect("turn B manager snapshot");
    assert_eq!(after_b.parent_run_id.as_deref(), Some("parent-run-B"));
    assert_eq!(after_b.parent_tool_call_id.as_deref(), Some("continue-B"));
    assert_eq!(
        after_b
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.final_answer.as_deref()),
        Some("turn B child done")
    );
    let metadata = child_metadata.lock().expect("current-turn child metadata");
    let turn_b_metadata = &metadata["B"][0];
    assert_eq!(turn_b_metadata["_vv_agent_trace_id"], "trace-B");
    assert_eq!(turn_b_metadata["_vv_agent_parent_run_id"], "parent-run-B");
    assert_eq!(
        turn_b_metadata["_vv_agent_parent_tool_call_id"],
        "continue-B"
    );
    assert_eq!(turn_b_metadata["_vv_agent_run_id"], turn_b_child_run_id);
    drop(metadata);

    let events_b_after_turn = events_b.lock().expect("turn B lifecycle events").len();
    let streams_b_after_turn = streams_b.lock().expect("turn B stream events").len();
    let token_c = vv_agent::CancellationToken::default();
    let events_c = Arc::new(Mutex::new(Vec::new()));
    let session_events_c = Arc::new(Mutex::new(Vec::new()));
    let streams_c = Arc::new(Mutex::new(Vec::new()));
    runtime.set_tool_policy(ToolPolicy::default());
    let turn_c = runtime
        .run_with_controls(
            current_turn_parent_task("C"),
            current_turn_controls(
                "C",
                token_c.clone(),
                manager.clone(),
                events_c.clone(),
                session_events_c.clone(),
                streams_c.clone(),
                None,
            ),
        )
        .expect("turn C starts async continuation");
    assert_eq!(turn_c.status, AgentStatus::Completed);
    turn_c_started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("turn C child entered LLM");
    token_c.cancel();
    assert!(!token_a.is_cancelled());
    assert!(!token_b.is_cancelled());
    let (released, wake) = &*turn_c_release;
    *released.lock().expect("turn C release lock") = true;
    wake.notify_all();
    assert!(manager.wait(&task_id, Some(Duration::from_secs(2))));

    assert_eq!(
        events_a.lock().expect("turn A lifecycle events").len(),
        events_a_after_initial
    );
    assert_eq!(
        stale_sink_invocations_a.load(Ordering::SeqCst),
        stale_sink_invocations_after_initial
    );
    assert_eq!(
        events_b.lock().expect("turn B lifecycle events").len(),
        events_b_after_turn
    );
    assert_eq!(
        streams_b.lock().expect("turn B stream events").len(),
        streams_b_after_turn
    );
    let turn_c_streams = streams_c.lock().expect("turn C stream events");
    assert_eq!(turn_c_streams.len(), 1);
    assert_eq!(turn_c_streams[0]["delta"], "turn C child delta");
    drop(turn_c_streams);
    let turn_c_events = events_c.lock().expect("turn C lifecycle events");
    assert_eq!(
        turn_c_events
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["sub_run_started", "sub_run_completed"]
    );
    for (_, payload) in turn_c_events.iter() {
        assert_eq!(payload["trace_id"], "trace-C");
        assert_eq!(payload["parent_run_id"], "parent-run-C");
        assert_eq!(payload["parent_tool_call_id"], "continue-C");
    }
    let turn_c_child_run_id = turn_c_events[0].1["run_id"]
        .as_str()
        .expect("turn C child run id")
        .to_string();
    assert_ne!(turn_c_child_run_id, turn_b_child_run_id);
    assert_eq!(turn_c_events[1].1["status"], "failed");
    assert_eq!(turn_c_events[1].1["error"], "Operation was cancelled");
    drop(turn_c_events);
    let after_c = manager.get(&task_id).expect("turn C manager snapshot");
    assert_eq!(after_c.parent_run_id.as_deref(), Some("parent-run-C"));
    assert_eq!(after_c.parent_tool_call_id.as_deref(), Some("continue-C"));
    let outcome = after_c.outcome.expect("turn C child outcome");
    assert_eq!(outcome.status, AgentStatus::Failed);
    assert_eq!(outcome.error.as_deref(), Some("Operation was cancelled"));
    assert_eq!(outcome.error_code.as_deref(), Some("sub_task_failed"));
    assert!(token_c.is_cancelled());
    assert!(!token_a.is_cancelled());

    let metadata = child_metadata.lock().expect("current-turn child metadata");
    assert_eq!(metadata["A"][0]["_vv_agent_tool_policy_approval"], "never");
    assert_eq!(metadata["B"][0]["_vv_agent_tool_policy_can_use_tool"], true);
    assert!(metadata["C"][0]
        .get("_vv_agent_tool_policy_approval")
        .is_none());
    assert!(metadata["C"][0]
        .get("_vv_agent_tool_policy_can_use_tool")
        .is_none());
    drop(metadata);
    assert_eq!(
        *session_events_c.lock().expect("turn C session events"),
        [
            "sub_agent_session_run_start".to_string(),
            "sub_agent_session_run_end".to_string(),
        ]
    );

    let session_events_b_before_plain = session_events_b
        .lock()
        .expect("turn B session events")
        .len();
    let session_events_c_before_plain = session_events_c
        .lock()
        .expect("turn C session events")
        .len();
    manager
        .continue_task(&task_id, "plain public continuation D")
        .expect("plain continuation after sidecar turns");
    assert!(manager.wait(&task_id, Some(Duration::from_secs(2))));

    assert_eq!(
        session_events_a
            .lock()
            .expect("turn A session events")
            .len(),
        session_events_a_after_initial
    );
    assert_eq!(
        stale_sink_invocations_a.load(Ordering::SeqCst),
        stale_sink_invocations_after_initial
    );
    assert_eq!(
        session_events_b
            .lock()
            .expect("turn B session events")
            .len(),
        session_events_b_before_plain
    );
    assert_eq!(
        session_events_c
            .lock()
            .expect("turn C session events")
            .len(),
        session_events_c_before_plain
    );
    let after_plain = manager.get(&task_id).expect("plain continuation snapshot");
    assert_eq!(after_plain.parent_run_id.as_deref(), Some("parent-run-A"));
    assert_eq!(
        after_plain.parent_tool_call_id.as_deref(),
        Some("delegate-A")
    );
    assert_eq!(
        after_plain
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.final_answer.as_deref()),
        Some("plain continuation done")
    );
    let metadata = child_metadata.lock().expect("current-turn child metadata");
    assert_eq!(metadata["D"][0]["_vv_agent_parent_run_id"], "parent-run-A");
    assert_eq!(
        metadata["D"][0]["_vv_agent_parent_tool_call_id"],
        "delegate-A"
    );
}
