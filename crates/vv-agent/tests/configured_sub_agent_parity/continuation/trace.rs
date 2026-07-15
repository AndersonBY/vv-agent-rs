use super::*;

struct SnapshotCaptureSession {
    task_id: String,
    snapshots: Arc<Mutex<Vec<SubTaskTurnSnapshot>>>,
}

impl SubAgentSession for SnapshotCaptureSession {
    fn steer(&self, _prompt: &str) -> Result<(), String> {
        Ok(())
    }

    fn continue_run_with_snapshot(
        &self,
        _prompt: &str,
        snapshot: SubTaskTurnSnapshot,
    ) -> Result<SubTaskOutcome, String> {
        self.snapshots
            .lock()
            .expect("captured continuation snapshots")
            .push(snapshot);
        Ok(SubTaskOutcome {
            task_id: self.task_id.clone(),
            agent_name: "researcher".to_string(),
            status: AgentStatus::Completed,
            session_id: None,
            final_answer: Some("captured".to_string()),
            wait_reason: None,
            error: None,
            error_code: None,
            completion_reason: None,
            completion_tool_name: None,
            partial_output: None,
            cycles: 1,
            todo_list: Vec::new(),
            resolved: BTreeMap::new(),
        })
    }
}

fn capture_task_metadata_continuation_trace(
    suffix: &str,
    task_metadata: BTreeMap<String, Value>,
) -> String {
    let task_id = format!("trace-task-{suffix}");
    let session_id = format!("trace-session-{suffix}");
    let snapshots = Arc::new(Mutex::new(Vec::new()));
    let manager = SubTaskManager::default();
    manager.attach_session(
        task_id.clone(),
        session_id,
        "researcher",
        "initial task",
        Arc::new(MemoryWorkspaceBackend::default()),
        Arc::new(SnapshotCaptureSession {
            task_id: task_id.clone(),
            snapshots: snapshots.clone(),
        }),
    );
    let llm = ScriptedLlmClient::from_steps(vec![
        ScriptStep::response(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                format!("continue-{suffix}"),
                "sub_task_status",
                json!({
                    "task_ids": [task_id],
                    "message": "continue for trace capture",
                    "wait_for_response": true
                }),
            )],
        )),
        ScriptStep::response(finish_response(
            &format!("parent-finish-{suffix}"),
            "parent done",
        )),
    ]);
    let mut parent = AgentTask::new(
        format!("trace-parent-{suffix}"),
        "shared-model",
        "Parent prompt",
        "Continue retained child",
    );
    parent.max_cycles = 3;
    parent.allow_interruption = false;
    parent.use_workspace = false;
    parent.extra_tool_names = vec!["sub_task_status".to_string()];
    parent.metadata = task_metadata;
    let result = AgentRuntime::new(llm)
        .with_tool_registry(build_default_registry())
        .run_with_controls(
            parent,
            RuntimeRunControls {
                execution_context: Some(ExecutionContext {
                    metadata: BTreeMap::from([
                        ("_vv_agent_trace_id".to_string(), json!(7)),
                        ("trace_id".to_string(), json!(true)),
                    ]),
                    ..ExecutionContext::default()
                }),
                run_context: Some(RunContext {
                    run_id: format!("trace-parent-run-{suffix}"),
                    agent_name: "parent".to_string(),
                    metadata: BTreeMap::from([("trace_id".to_string(), json!(["invalid"]))]),
                    ..RunContext::default()
                }),
                sub_task_manager: Some(manager),
                ..RuntimeRunControls::default()
            },
        )
        .expect("parent continuation trace run");
    assert_eq!(result.status, AgentStatus::Completed);
    let trace_id = snapshots
        .lock()
        .expect("captured continuation snapshots")
        .first()
        .and_then(|snapshot| snapshot.trace_id.clone())
        .expect("captured continuation trace");
    trace_id
}

#[test]
fn continuation_snapshot_trace_falls_back_to_parent_task_metadata() {
    let fixture = contract();
    assert_eq!(
        fixture["identity"]["trace_precedence"],
        json!([
            "execution_context",
            "run_context",
            "task_metadata",
            "child_run_id"
        ])
    );
    assert_eq!(
        fixture["identity"]["non_string_metadata_policy"],
        "ignore_and_fall_through"
    );

    assert_eq!(
        capture_task_metadata_continuation_trace(
            "reserved",
            BTreeMap::from([
                (
                    "_vv_agent_trace_id".to_string(),
                    json!("task-reserved-trace")
                ),
                ("trace_id".to_string(), json!("task-public-trace")),
            ]),
        ),
        "task-reserved-trace"
    );
    assert_eq!(
        capture_task_metadata_continuation_trace(
            "public",
            BTreeMap::from([
                ("_vv_agent_trace_id".to_string(), json!(7)),
                ("trace_id".to_string(), json!("task-public-trace")),
            ]),
        ),
        "task-public-trace"
    );
}
