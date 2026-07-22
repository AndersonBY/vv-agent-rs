use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use vv_agent::{
    Agent, AgentStatus, ApprovalDecision, ApprovalFuture, ApprovalPolicy, ApprovalProvider,
    ApprovalRequest, ApprovalRequirement, CompletionReason, FunctionTool, LLMResponse,
    MemorySession, ModelRef, RunConfig, Runner, ScriptStep, ScriptedModelProvider, Session,
    ToolCall, ToolContext, ToolExposure, ToolOutput, ToolPolicy, ToolRunContext, ToolUseBehavior,
};

#[test]
fn function_tool_approval_defaults_false_and_predicate_uses_context_and_arguments() {
    let default_tool = test_tool("default_tool", None);
    let default_executor = default_tool.to_executor();
    let default_call = ToolCall::from_raw_arguments("default_call", "default_tool", json!({}));
    let mut context = ToolContext::new("./workspace");

    assert_eq!(
        default_executor.approval_requirement(&default_call, &ToolRunContext::new(&mut context)),
        ApprovalRequirement::NotRequired
    );

    let predicate_tool = FunctionTool::builder("conditional_tool")
        .json_schema(json!({
            "type": "object",
            "properties": {
                "destructive": {"type": "boolean"}
            },
            "required": ["destructive"]
        }))
        .needs_approval_if(|context, arguments| {
            context.metadata.get("scope") == Some(&json!("protected"))
                && arguments.get("destructive") == Some(&json!(true))
        })
        .handler(|_context, _arguments: Value| async { Ok(ToolOutput::text("ran")) })
        .build()
        .expect("conditional tool");
    let predicate_executor = predicate_tool.to_executor();
    let safe_call = ToolCall::from_raw_arguments(
        "safe_call",
        "conditional_tool",
        json!({"destructive": false}),
    );
    let destructive_call = ToolCall::from_raw_arguments(
        "destructive_call",
        "conditional_tool",
        json!({"destructive": true}),
    );
    context
        .metadata
        .insert("scope".to_string(), json!("protected"));

    assert_eq!(
        predicate_executor.approval_requirement(&safe_call, &ToolRunContext::new(&mut context)),
        ApprovalRequirement::NotRequired
    );
    assert_eq!(
        predicate_executor
            .approval_requirement(&destructive_call, &ToolRunContext::new(&mut context)),
        ApprovalRequirement::Required
    );
}

#[tokio::test]
async fn on_request_static_approval_interrupts_before_handler_and_resume_executes_once() {
    let executions = Arc::new(AtomicUsize::new(0));
    let executions_for_tool = executions.clone();
    let tool = FunctionTool::builder("delete_file")
        .json_schema(json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"}
            },
            "required": ["path"]
        }))
        .needs_approval(true)
        .handler(move |_context, _arguments: Value| {
            let executions = executions_for_tool.clone();
            async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text("deleted"))
            }
        })
        .build()
        .expect("delete tool");
    let (runner, agent) = runner_and_agent(
        tool,
        ApprovalPolicy::OnRequest,
        vec![
            LLMResponse::with_tool_calls(
                "delete",
                vec![ToolCall::from_raw_arguments(
                    "delete_call",
                    "delete_file",
                    json!({"path": "danger.txt"}),
                )],
            ),
            finish_response("deleted"),
        ],
        ToolUseBehavior::RunLlmAgain,
    );

    let result = runner.run(&agent, "delete").await.expect("run");

    assert_eq!(result.status(), AgentStatus::WaitUser);
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    let interruption_id = result
        .result()
        .cycles
        .iter()
        .flat_map(|cycle| cycle.tool_results.iter())
        .find_map(|tool_result| {
            tool_result
                .metadata
                .get("approval_interruption_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .expect("approval interruption");
    let mut state = result.into_state().expect("run state");
    state.approve(&interruption_id).expect("approve");

    let resumed = runner.resume(state).await.expect("resume");

    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_eq!(resumed.final_output(), Some("deleted"));
    assert_eq!(
        resumed.completion_reason(),
        Some(CompletionReason::ToolFinish)
    );
    assert_eq!(resumed.completion_tool_name(), Some("task_finish"));
    assert_eq!(resumed.result().cycles.len(), 1);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn approval_resume_preserves_explicit_wait_and_finish_actions() {
    for (tool_name, arguments, expected_status, expected_reason, expected_output) in [
        (
            "ask_user",
            json!({"question": "Choose after approval"}),
            AgentStatus::WaitUser,
            CompletionReason::WaitUser,
            "Choose after approval",
        ),
        (
            "task_finish",
            json!({"message": "finished after approval"}),
            AgentStatus::Completed,
            CompletionReason::ToolFinish,
            "finished after approval",
        ),
    ] {
        let provider = ScriptedModelProvider::new(
            "scripted",
            "approval-model",
            vec![LLMResponse::with_tool_calls(
                "assistant text before approved action",
                vec![ToolCall::from_raw_arguments(
                    "approved_action",
                    tool_name,
                    arguments,
                )],
            )],
        );
        let runner = Runner::builder()
            .model_provider(provider)
            .workspace("./workspace")
            .build()
            .expect("runner");
        let agent = Agent::builder("approval_agent")
            .instructions("Use the control tool.")
            .model(ModelRef::named("approval-model"))
            .tool_policy(approval_policy(ApprovalPolicy::Always))
            .build()
            .expect("agent");
        let interrupted = runner.run(&agent, "run").await.expect("interrupted");
        let interruption_id = interrupted.approvals()[0].interruption_id.clone();
        let mut state = interrupted.into_state().expect("state");
        state.approve(&interruption_id).expect("approve");

        let resumed = runner.resume(state).await.expect("resume");

        assert_eq!(resumed.status(), expected_status, "{tool_name}");
        assert_eq!(
            resumed.completion_reason(),
            Some(expected_reason),
            "{tool_name}"
        );
        assert_eq!(resumed.completion_tool_name(), Some(tool_name));
        assert_eq!(resumed.final_output(), Some(expected_output), "{tool_name}");
        if expected_status == AgentStatus::WaitUser {
            assert_eq!(
                resumed.partial_output(),
                Some("assistant text before approved action")
            );
        } else {
            assert_eq!(resumed.partial_output(), None);
        }
    }
}

#[tokio::test]
async fn approval_resume_applies_stop_on_first_and_stop_at_tool_names() {
    for (behavior, expected_reason) in [
        (
            ToolUseBehavior::StopOnFirstTool,
            CompletionReason::StopOnFirstTool,
        ),
        (
            ToolUseBehavior::StopAtToolNames(vec!["guarded_lookup".to_string()]),
            CompletionReason::StopAtToolName,
        ),
    ] {
        let tool = FunctionTool::builder("guarded_lookup")
            .needs_approval(true)
            .handler(|_context, _arguments: Value| async {
                Ok(ToolOutput::text("approved lookup result"))
            })
            .build()
            .expect("tool");
        let (runner, agent) = runner_and_agent(
            tool,
            ApprovalPolicy::OnRequest,
            vec![single_tool_response("guarded_lookup")],
            behavior,
        );
        let interrupted = runner.run(&agent, "lookup").await.expect("interrupted");
        let interruption_id = interrupted.approvals()[0].interruption_id.clone();
        let mut state = interrupted.into_state().expect("state");
        state.approve(&interruption_id).expect("approve");

        let resumed = runner.resume(state).await.expect("resume");

        assert_eq!(resumed.status(), AgentStatus::Completed);
        assert_eq!(resumed.completion_reason(), Some(expected_reason));
        assert_eq!(resumed.completion_tool_name(), Some("guarded_lookup"));
        assert_eq!(resumed.final_output(), Some("approved lookup result"));
        assert_eq!(resumed.partial_output(), None);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloned_manual_approval_state_executes_once_and_persists_one_result() {
    let executions = Arc::new(AtomicUsize::new(0));
    let executions_for_tool = executions.clone();
    let tool = FunctionTool::builder("guarded_once")
        .json_schema(json!({
            "type": "object",
            "properties": {
                "nested": {
                    "type": "object",
                    "properties": {
                        "value": {"type": "string"}
                    },
                    "required": ["value"]
                }
            },
            "required": ["nested"]
        }))
        .needs_approval(true)
        .handler(move |_context, arguments: Value| {
            let executions = executions_for_tool.clone();
            async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text(
                    arguments["nested"]["value"].as_str().unwrap_or_default(),
                ))
            }
        })
        .build()
        .expect("guarded tool");
    let (runner, agent) = runner_and_agent(
        tool,
        ApprovalPolicy::OnRequest,
        vec![
            LLMResponse::with_tool_calls(
                "run once",
                vec![ToolCall::from_raw_arguments(
                    "guarded_once_call",
                    "guarded_once",
                    json!({"nested": {"value": "original"}}),
                )],
            ),
            finish_response("original"),
        ],
        ToolUseBehavior::RunLlmAgain,
    );
    let session = MemorySession::new("approval-once");
    let interrupted = runner
        .run_with_config(
            &agent,
            "run once",
            RunConfig::builder().session(session.clone()).build(),
        )
        .await
        .expect("run");
    let interruption_id = interrupted.result().cycles[0].tool_results[0].metadata
        ["approval_interruption_id"]
        .as_str()
        .expect("interruption id")
        .to_string();
    let mut state = interrupted.into_state().expect("state");
    state.approve(&interruption_id).expect("approve");
    let state_clone = state.clone();
    let first_runner = runner.clone();
    let second_runner = runner.clone();

    let (first, second) = tokio::join!(
        async move { first_runner.resume(state).await },
        async move { second_runner.resume(state_clone).await },
    );
    let outcomes = [first, second];
    let resumed = outcomes
        .iter()
        .find_map(|outcome| outcome.as_ref().ok())
        .expect("one successful resume");
    let errors = outcomes
        .iter()
        .filter_map(|outcome| outcome.as_ref().err())
        .collect::<Vec<_>>();

    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0], "approval_already_consumed");
    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_eq!(resumed.final_output(), Some("original"));
    assert!(resumed.result().wait_reason.is_none());
    assert!(resumed.result().error.is_none());
    assert_eq!(resumed.metadata()["resumed"], json!(true));
    assert_eq!(
        resumed.metadata()["approved_interruption_id"],
        json!(interruption_id)
    );
    let session_items = session.get_items(None).await.expect("session items");
    let tool_results = session_items
        .iter()
        .filter(|item| {
            let message = item.to_message();
            message.role == vv_agent::MessageRole::Tool
                && message.tool_call_id.as_deref() == Some("guarded_once_call")
        })
        .count();
    assert_eq!(tool_results, 1);
}

#[tokio::test]
async fn on_request_predicate_gates_only_matching_runtime_call() {
    let executed_modes = Arc::new(Mutex::new(Vec::new()));
    let executed_modes_for_tool = executed_modes.clone();
    let tool = FunctionTool::builder("change_setting")
        .json_schema(json!({
            "type": "object",
            "properties": {
                "mode": {"type": "string"}
            },
            "required": ["mode"]
        }))
        .needs_approval_if(|context, arguments| {
            context.metadata.get("scope") == Some(&json!("protected"))
                && arguments.get("mode") == Some(&json!("destructive"))
        })
        .handler(move |_context, arguments: Value| {
            let executed_modes = executed_modes_for_tool.clone();
            async move {
                executed_modes
                    .lock()
                    .expect("executed modes")
                    .push(arguments["mode"].as_str().unwrap_or_default().to_string());
                Ok(ToolOutput::text("changed"))
            }
        })
        .build()
        .expect("setting tool");
    let (runner, agent) = runner_and_agent(
        tool,
        ApprovalPolicy::OnRequest,
        vec![LLMResponse::with_tool_calls(
            "change settings",
            vec![
                ToolCall::from_raw_arguments(
                    "safe_call",
                    "change_setting",
                    json!({"mode": "safe"}),
                ),
                ToolCall::from_raw_arguments(
                    "destructive_call",
                    "change_setting",
                    json!({"mode": "destructive"}),
                ),
            ],
        )],
        ToolUseBehavior::RunLlmAgain,
    );
    let agent = Agent::builder("approval_agent")
        .instructions("Use the requested tool.")
        .model(ModelRef::backend("scripted", "approval-model"))
        .tool_arc(agent.tools()[0].clone())
        .tool_policy(approval_policy(ApprovalPolicy::OnRequest))
        .metadata("scope", json!("protected"))
        .build()
        .expect("agent");

    let result = runner.run(&agent, "change").await.expect("run");

    assert_eq!(result.status(), AgentStatus::WaitUser);
    assert_eq!(
        executed_modes.lock().expect("executed modes").as_slice(),
        &["safe".to_string()]
    );
}

#[tokio::test]
async fn run_never_overrides_agent_always_and_tool_declaration() {
    let executions = Arc::new(AtomicUsize::new(0));
    let executions_for_tool = executions.clone();
    let tool = FunctionTool::builder("declared_tool")
        .needs_approval(true)
        .handler(move |_context, _arguments: Value| {
            let executions = executions_for_tool.clone();
            async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text("ran"))
            }
        })
        .build()
        .expect("declared tool");
    let (runner, agent) = runner_and_agent(
        tool,
        ApprovalPolicy::Always,
        vec![single_tool_response("declared_tool")],
        ToolUseBehavior::StopOnFirstTool,
    );
    let approval_requests = Arc::new(AtomicUsize::new(0));

    let result = runner
        .run_with_config(
            &agent,
            "run",
            RunConfig::builder()
                .approval_provider(Arc::new(ImmediateApproval {
                    requests: approval_requests.clone(),
                }))
                .tool_policy(approval_policy(ApprovalPolicy::Never))
                .build(),
        )
        .await
        .expect("run");

    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(approval_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn default_policy_respects_tool_needs_approval() {
    let executions = Arc::new(AtomicUsize::new(0));
    let executions_for_tool = executions.clone();
    let tool = FunctionTool::builder("declared_tool")
        .needs_approval(true)
        .handler(move |_context, _arguments: Value| {
            let executions = executions_for_tool.clone();
            async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text("ran"))
            }
        })
        .build()
        .expect("declared tool");
    let provider = ScriptedModelProvider::new(
        "scripted",
        "approval-model",
        vec![single_tool_response("declared_tool")],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("approval_agent")
        .instructions("Use the requested tool.")
        .model(ModelRef::backend("scripted", "approval-model"))
        .tool(tool)
        .tool_use_behavior(ToolUseBehavior::StopOnFirstTool)
        .build()
        .expect("agent");
    let approval_requests = Arc::new(AtomicUsize::new(0));

    let result = runner
        .run_with_config(
            &agent,
            "run",
            RunConfig::builder()
                .approval_provider(Arc::new(ImmediateApproval {
                    requests: approval_requests.clone(),
                }))
                .build(),
        )
        .await
        .expect("run");

    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(approval_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn always_forces_default_false_tool_through_live_provider() {
    let executions = Arc::new(AtomicUsize::new(0));
    let executions_for_tool = executions.clone();
    let tool = FunctionTool::builder("default_tool")
        .handler(move |_context, _arguments: Value| {
            let executions = executions_for_tool.clone();
            async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text("ran"))
            }
        })
        .build()
        .expect("default tool");
    let (runner, agent) = runner_and_agent(
        tool,
        ApprovalPolicy::Always,
        vec![single_tool_response("default_tool")],
        ToolUseBehavior::StopOnFirstTool,
    );
    let approval_requests = Arc::new(AtomicUsize::new(0));

    let result = runner
        .run_with_config(
            &agent,
            "run",
            RunConfig::builder()
                .approval_provider(Arc::new(ImmediateApproval {
                    requests: approval_requests.clone(),
                }))
                .build(),
        )
        .await
        .expect("run");

    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(approval_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn on_request_default_false_tool_does_not_reach_live_provider() {
    let tool = test_tool("default_tool", None);
    let (runner, agent) = runner_and_agent(
        tool,
        ApprovalPolicy::OnRequest,
        vec![single_tool_response("default_tool")],
        ToolUseBehavior::StopOnFirstTool,
    );
    let approval_requests = Arc::new(AtomicUsize::new(0));

    let result = runner
        .run_with_config(
            &agent,
            "run",
            RunConfig::builder()
                .approval_provider(Arc::new(ImmediateApproval {
                    requests: approval_requests.clone(),
                }))
                .build(),
        )
        .await
        .expect("run");

    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(approval_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn layered_argument_policies_are_anded_and_denial_precedes_approval() {
    let executions = Arc::new(AtomicUsize::new(0));
    let executions_for_tool = executions.clone();
    let tool = FunctionTool::builder("guarded_tool")
        .json_schema(json!({
            "type": "object",
            "properties": {
                "scope": {"type": "string"}
            },
            "required": ["scope"]
        }))
        .needs_approval(true)
        .handler(move |_context, _arguments: Value| {
            let executions = executions_for_tool.clone();
            async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text("ran"))
            }
        })
        .build()
        .expect("guarded tool");
    let provider = ScriptedModelProvider::new(
        "scripted",
        "approval-model",
        vec![LLMResponse::with_tool_calls(
            "guarded calls",
            ["agent", "runner", "run", "allowed"]
                .into_iter()
                .map(|scope| {
                    ToolCall::from_raw_arguments(
                        format!("{scope}_call"),
                        "guarded_tool",
                        json!({"scope": scope}),
                    )
                })
                .collect(),
        )],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .default_run_config(
            RunConfig::builder()
                .tool_policy(ToolPolicy::default().can_use_tool(|_name, arguments| {
                    arguments.get("scope") != Some(&json!("runner"))
                }))
                .build(),
        )
        .build()
        .expect("runner");
    let agent = Agent::builder("approval_agent")
        .instructions("Use the guarded tool.")
        .model(ModelRef::backend("scripted", "approval-model"))
        .tool(tool)
        .tool_policy(
            ToolPolicy::default()
                .can_use_tool(|_name, arguments| arguments.get("scope") != Some(&json!("agent")))
                .allow_only(["task_finish", "guarded_tool"]),
        )
        .max_cycles(1)
        .build()
        .expect("agent");
    let approval_requests = Arc::new(AtomicUsize::new(0));

    let result =
        runner
            .run_with_config(
                &agent,
                "run",
                RunConfig::builder()
                    .approval_provider(Arc::new(ImmediateApproval {
                        requests: approval_requests.clone(),
                    }))
                    .tool_policy(ToolPolicy::default().can_use_tool(|_name, arguments| {
                        arguments.get("scope") != Some(&json!("run"))
                    }))
                    .build(),
            )
            .await
            .expect("run");

    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(approval_requests.load(Ordering::SeqCst), 1);
    let tool_results = &result.result().cycles[0].tool_results;
    assert_eq!(tool_results.len(), 4);
    for result in &tool_results[..3] {
        assert_eq!(result.error_code.as_deref(), Some("tool_not_allowed"));
        assert_eq!(result.metadata["policy_source"], json!("can_use_tool"));
    }
    assert_eq!(tool_results[3].status, vv_agent::ToolResultStatus::Success);
}

#[tokio::test]
async fn hidden_and_unknown_forced_calls_never_reach_approval_or_execution() {
    let executions = Arc::new(AtomicUsize::new(0));
    let executions_for_tool = executions.clone();
    let hidden = FunctionTool::builder("hidden_action")
        .exposure(ToolExposure::Hidden)
        .needs_approval(true)
        .handler(move |_context, _arguments: Value| {
            let executions = executions_for_tool.clone();
            async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text("unexpected"))
            }
        })
        .build()
        .expect("hidden tool");
    let provider = ScriptedModelProvider::new(
        "scripted",
        "approval-model",
        vec![LLMResponse::with_tool_calls(
            "forced calls",
            vec![
                ToolCall::from_raw_arguments("hidden_call", "hidden_action", json!({})),
                ToolCall::from_raw_arguments("unknown_call", "unknown_action", json!({})),
            ],
        )],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("approval_agent")
        .instructions("Use tools.")
        .model(ModelRef::backend("scripted", "approval-model"))
        .tool(hidden)
        .max_cycles(1)
        .build()
        .expect("agent");
    let approval_requests = Arc::new(AtomicUsize::new(0));

    let result = runner
        .run_with_config(
            &agent,
            "run",
            RunConfig::builder()
                .approval_provider(Arc::new(ImmediateApproval {
                    requests: approval_requests.clone(),
                }))
                .tool_policy(approval_policy(ApprovalPolicy::Always))
                .build(),
        )
        .await
        .expect("run");

    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert_eq!(approval_requests.load(Ordering::SeqCst), 0);
    assert!(result.result().cycles[0]
        .tool_results
        .iter()
        .all(|tool_result| {
            tool_result.error_code.as_deref() == Some("tool_not_allowed")
                && tool_result.metadata["policy_source"] == json!("planned_name")
        }));
    assert!(result.result().cycles[0]
        .tool_results
        .iter()
        .all(|tool_result| !tool_result
            .metadata
            .contains_key("approval_interruption_id")));
}

#[tokio::test]
async fn always_and_never_do_not_evaluate_dynamic_approval_predicates() {
    for (policy, tool_name, expected_requests) in [
        (ApprovalPolicy::Always, "always_guarded", 1),
        (ApprovalPolicy::Never, "never_guarded", 0),
    ] {
        let predicate_calls = Arc::new(AtomicUsize::new(0));
        let predicate_calls_for_tool = predicate_calls.clone();
        let executions = Arc::new(AtomicUsize::new(0));
        let executions_for_tool = executions.clone();
        let tool = FunctionTool::builder(tool_name)
            .needs_approval_if(move |_context, _arguments| {
                predicate_calls_for_tool.fetch_add(1, Ordering::SeqCst);
                panic!("{policy:?} must short-circuit the tool approval predicate")
            })
            .handler(move |_context, _arguments: Value| {
                let executions = executions_for_tool.clone();
                async move {
                    executions.fetch_add(1, Ordering::SeqCst);
                    Ok(ToolOutput::text("ran"))
                }
            })
            .build()
            .expect("guarded tool");
        let (runner, agent) = runner_and_agent(
            tool,
            policy,
            vec![single_tool_response(tool_name)],
            ToolUseBehavior::StopOnFirstTool,
        );
        let approval_requests = Arc::new(AtomicUsize::new(0));

        let result = runner
            .run_with_config(
                &agent,
                "run",
                RunConfig::builder()
                    .approval_provider(Arc::new(ImmediateApproval {
                        requests: approval_requests.clone(),
                    }))
                    .build(),
            )
            .await
            .expect("run");

        assert_eq!(result.status(), AgentStatus::Completed);
        assert_eq!(predicate_calls.load(Ordering::SeqCst), 0);
        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert_eq!(approval_requests.load(Ordering::SeqCst), expected_requests);
    }
}

#[tokio::test]
async fn default_and_on_request_evaluate_dynamic_approval_predicates() {
    for (policy, tool_name) in [
        (ApprovalPolicy::Default, "default_guarded"),
        (ApprovalPolicy::OnRequest, "on_request_guarded"),
    ] {
        let predicate_calls = Arc::new(AtomicUsize::new(0));
        let predicate_calls_for_tool = predicate_calls.clone();
        let executions = Arc::new(AtomicUsize::new(0));
        let executions_for_tool = executions.clone();
        let tool = FunctionTool::builder(tool_name)
            .needs_approval_if(move |_context, _arguments| {
                predicate_calls_for_tool.fetch_add(1, Ordering::SeqCst);
                true
            })
            .handler(move |_context, _arguments: Value| {
                let executions = executions_for_tool.clone();
                async move {
                    executions.fetch_add(1, Ordering::SeqCst);
                    Ok(ToolOutput::text("ran"))
                }
            })
            .build()
            .expect("guarded tool");
        let (runner, agent) = runner_and_agent(
            tool,
            policy,
            vec![single_tool_response(tool_name)],
            ToolUseBehavior::StopOnFirstTool,
        );
        let approval_requests = Arc::new(AtomicUsize::new(0));

        let result = runner
            .run_with_config(
                &agent,
                "run",
                RunConfig::builder()
                    .approval_provider(Arc::new(ImmediateApproval {
                        requests: approval_requests.clone(),
                    }))
                    .build(),
            )
            .await
            .expect("run");

        assert_eq!(result.status(), AgentStatus::Completed);
        assert_eq!(predicate_calls.load(Ordering::SeqCst), 1);
        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert_eq!(approval_requests.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn manual_approval_policy_denial_is_returned_to_llm_without_execution() {
    let executions = Arc::new(AtomicUsize::new(0));
    let executions_for_tool = executions.clone();
    let allowed = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let allowed_for_policy = allowed.clone();
    let tool = FunctionTool::builder("guarded_action")
        .needs_approval(true)
        .handler(move |_context, _arguments: Value| {
            let executions = executions_for_tool.clone();
            async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text("executed"))
            }
        })
        .build()
        .expect("guarded tool");
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "approval-model",
        vec![
            ScriptStep::callback(|_request| Ok(single_tool_response("guarded_action"))),
            ScriptStep::callback(|request| {
                assert!(request.messages.iter().any(|message| {
                    message.role == vv_agent::MessageRole::Tool
                        && message.tool_call_id.as_deref() == Some("tool_call")
                        && message.content.contains("not allowed")
                }));
                Ok(finish_response("policy denial handled"))
            }),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("approval_agent")
        .instructions("Use the guarded tool.")
        .model(ModelRef::backend("scripted", "approval-model"))
        .tool(tool)
        .tool_policy(
            ToolPolicy {
                approval: ApprovalPolicy::Default,
                ..ToolPolicy::default()
            }
            .can_use_tool(move |name, _arguments| {
                name != "guarded_action" || allowed_for_policy.load(Ordering::SeqCst)
            }),
        )
        .build()
        .expect("agent");

    let interrupted = runner.run(&agent, "run").await.expect("run");
    assert_eq!(interrupted.status(), AgentStatus::WaitUser);
    let interruption_id = interrupted.result().cycles[0].tool_results[0].metadata
        ["approval_interruption_id"]
        .as_str()
        .expect("interruption id")
        .to_string();
    let mut state = interrupted.into_state().expect("state");
    state.approve(&interruption_id).expect("approve");
    allowed.store(false, Ordering::SeqCst);

    let resumed = runner
        .resume(state)
        .await
        .expect("resume after policy denial");

    assert_eq!(resumed.status(), AgentStatus::Completed);
    assert_eq!(resumed.final_output(), Some("policy denial handled"));
    assert_eq!(resumed.result().cycles.len(), 1);
    assert_eq!(executions.load(Ordering::SeqCst), 0);
}

struct ImmediateApproval {
    requests: Arc<AtomicUsize>,
}

impl ApprovalProvider for ImmediateApproval {
    fn should_request(&self, _request: &ApprovalRequest) -> bool {
        self.requests.fetch_add(1, Ordering::SeqCst);
        true
    }

    fn decide(&self, _request: &ApprovalRequest) -> ApprovalFuture<Option<ApprovalDecision>> {
        Box::pin(async { Ok(Some(ApprovalDecision::allow())) })
    }
}

fn runner_and_agent(
    tool: FunctionTool<Value>,
    approval: ApprovalPolicy,
    responses: Vec<LLMResponse>,
    tool_use_behavior: ToolUseBehavior,
) -> (Runner, Agent) {
    let provider = ScriptedModelProvider::new("scripted", "approval-model", responses);
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("approval_agent")
        .instructions("Use the requested tool.")
        .model(ModelRef::backend("scripted", "approval-model"))
        .tool(tool)
        .tool_policy(approval_policy(approval))
        .tool_use_behavior(tool_use_behavior)
        .build()
        .expect("agent");
    (runner, agent)
}

fn test_tool(name: &str, needs_approval: Option<bool>) -> FunctionTool<Value> {
    let builder = FunctionTool::builder(name);
    let builder = match needs_approval {
        Some(needs_approval) => builder.needs_approval(needs_approval),
        None => builder,
    };
    builder
        .handler(|_context, _arguments: Value| async { Ok(ToolOutput::text("ran")) })
        .build()
        .expect("tool")
}

fn approval_policy(approval: ApprovalPolicy) -> ToolPolicy {
    ToolPolicy {
        approval,
        ..ToolPolicy::default()
    }
}

fn single_tool_response(tool_name: &str) -> LLMResponse {
    LLMResponse::with_tool_calls(
        "run tool",
        vec![ToolCall::from_raw_arguments(
            "tool_call",
            tool_name,
            json!({}),
        )],
    )
}

fn finish_response(message: &str) -> LLMResponse {
    LLMResponse::with_tool_calls(
        "finish",
        vec![ToolCall::from_raw_arguments(
            "finish_call",
            "task_finish",
            json!({"message": message}),
        )],
    )
}
