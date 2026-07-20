use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use vv_agent::runtime::{ExecutionContext, RuntimeRunControls};
use vv_agent::{
    build_default_registry, AgentRuntime, AgentStatus, AgentTask, ApprovalBroker, ApprovalDecision,
    ApprovalFuture, ApprovalPolicy, ApprovalProvider, ApprovalRequest, FunctionTool, LLMResponse,
    ScriptStep, ScriptedLlmClient, SubAgentConfig, Tool, ToolCall, ToolOutput, ToolPolicy,
};

const CHILD_AGENT: &str = "policy_child";
const SHARED_MODEL: &str = "policy-model";

fn policy_contract() -> Value {
    super::contract()["tool_policy_projection"].clone()
}

fn configured_parent(extra_tool_names: &[&str], child_exclusions: &[&str]) -> AgentTask {
    let mut parent = AgentTask::new(
        "parent-policy-task",
        SHARED_MODEL,
        "Delegate to the configured child.",
        "Exercise the child tool policy.",
    );
    parent.max_cycles = 2;
    parent.allow_interruption = false;
    parent.use_workspace = false;
    parent.extra_tool_names = extra_tool_names
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    let mut child = SubAgentConfig::new(SHARED_MODEL, "Exercise inherited policy safely.");
    child.max_cycles = 2;
    child.exclude_tools = child_exclusions
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    parent.sub_agents.insert(CHILD_AGENT.to_string(), child);
    parent
}

fn delegate_response() -> LLMResponse {
    LLMResponse::with_tool_calls(
        "delegate",
        vec![ToolCall::from_raw_arguments(
            "delegate-call",
            "create_sub_task",
            json!({
                "agent_id": CHILD_AGENT,
                "task_description": "Attempt the requested child action"
            }),
        )],
    )
}

fn finish_response(call_id: &str, message: &str) -> LLMResponse {
    LLMResponse::with_tool_calls(
        "finish",
        vec![ToolCall::from_raw_arguments(
            call_id,
            "task_finish",
            json!({"message": message}),
        )],
    )
}

fn schema_names(request: &vv_agent::LlmRequest) -> Vec<String> {
    request
        .tools
        .iter()
        .filter_map(|schema| schema["function"]["name"].as_str().map(str::to_string))
        .collect()
}

fn register_counting_tool(
    registry: &mut vv_agent::ToolRegistry,
    name: &'static str,
    needs_approval: bool,
    executions: Arc<AtomicUsize>,
    execution_order: Option<Arc<Mutex<Vec<String>>>>,
) {
    let tool = FunctionTool::builder(name)
        .metadata("configured_tool", json!(name))
        .needs_approval(needs_approval)
        .handler(move |_context, _arguments: Value| {
            let executions = executions.clone();
            let execution_order = execution_order.clone();
            async move {
                executions.fetch_add(1, Ordering::SeqCst);
                if let Some(execution_order) = execution_order {
                    execution_order
                        .lock()
                        .expect("execution order")
                        .push("executor".to_string());
                }
                Ok(ToolOutput::text("executed"))
            }
        })
        .build()
        .expect("counting tool");
    registry
        .register(tool.as_tool_spec())
        .expect("register counting tool");
}

#[test]
fn configured_child_hides_policy_tools_and_blocks_malicious_forced_calls() {
    let contract = policy_contract();
    assert_eq!(
        contract["inherited"],
        json!([
            "allowed_tools",
            "approval",
            "can_use_tool",
            "disallowed_tools",
            "denied_side_effects",
            "denied_capability_tags",
            "deny_terminal_tools",
            "denied_cost_dimensions"
        ])
    );
    assert_eq!(
        contract["execution_order"],
        json!([
            "planned_name",
            "allowed_tools",
            "disallowed_tools",
            "can_use_tool",
            "tool_lookup",
            "metadata_denials",
            "approval",
            "executor"
        ])
    );

    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = build_default_registry();
    for name in ["child_excluded", "not_allowed", "explicitly_disallowed"] {
        register_counting_tool(&mut registry, name, false, executions.clone(), None);
    }

    let captured_child_schemas = Arc::new(Mutex::new(Vec::new()));
    let schemas_for_child = captured_child_schemas.clone();
    let llm = ScriptedLlmClient::from_steps(vec![
        ScriptStep::response(delegate_response()),
        ScriptStep::callback(move |request| {
            assert_eq!(request.metadata["is_sub_task"], json!(true));
            *schemas_for_child.lock().expect("child schemas") = schema_names(request);
            Ok(LLMResponse::with_tool_calls(
                "force hidden tools",
                vec![
                    ToolCall::from_raw_arguments("excluded", "child_excluded", json!({})),
                    ToolCall::from_raw_arguments("outside-allow", "not_allowed", json!({})),
                    ToolCall::from_raw_arguments("disallowed", "explicitly_disallowed", json!({})),
                    ToolCall::from_raw_arguments(
                        "recursive",
                        "create_sub_task",
                        json!({
                            "agent_id": CHILD_AGENT,
                            "task_description": "escape child policy"
                        }),
                    ),
                ],
            ))
        }),
        ScriptStep::response(finish_response("child-finish", "child done")),
        ScriptStep::response(finish_response("parent-finish", "parent done")),
    ]);
    let mut runtime = AgentRuntime::new(llm).with_tool_registry(registry);
    runtime.set_tool_policy(
        ToolPolicy {
            approval: ApprovalPolicy::Never,
            ..ToolPolicy::default()
        }
        .allow_only([
            "task_finish",
            "create_sub_task",
            "sub_task_status",
            "child_excluded",
            "explicitly_disallowed",
        ])
        .disallow("explicitly_disallowed"),
    );

    let result = runtime
        .run(configured_parent(
            &["child_excluded", "not_allowed", "explicitly_disallowed"],
            &["child_excluded"],
        ))
        .expect("configured child run");

    assert_eq!(result.status, AgentStatus::Completed);
    let schemas = captured_child_schemas.lock().expect("child schemas");
    let policy_hidden = ["child_excluded", "not_allowed", "explicitly_disallowed"]
        .iter()
        .all(|name| !schemas.iter().any(|schema_name| schema_name == name));
    let builtins_hidden = contract["always_disallowed_for_child"]
        .as_array()
        .expect("always-disallowed child tools")
        .iter()
        .filter_map(Value::as_str)
        .all(|name| !schemas.iter().any(|schema_name| schema_name == name));
    assert_eq!(
        policy_hidden && builtins_hidden && executions.load(Ordering::SeqCst) == 0,
        contract["schema_and_execution_enforced"]
            .as_bool()
            .expect("schema and execution contract")
    );
    assert_eq!(
        !schemas.iter().any(|name| name == "child_excluded"),
        contract["child_may_only_tighten"]
            .as_bool()
            .expect("child tightening contract")
    );
}

#[derive(Clone)]
struct RecordingApprovalProvider {
    requests: Arc<Mutex<Vec<String>>>,
    execution_order: Option<Arc<Mutex<Vec<String>>>>,
}

impl ApprovalProvider for RecordingApprovalProvider {
    fn should_request(&self, request: &ApprovalRequest) -> bool {
        self.requests
            .lock()
            .expect("approval requests")
            .push(request.tool_name.clone());
        if request.tool_name == "guarded_action" {
            if let Some(execution_order) = self.execution_order.as_ref() {
                execution_order
                    .lock()
                    .expect("execution order")
                    .push("approval".to_string());
            }
        }
        true
    }

    fn decide(&self, _request: &ApprovalRequest) -> ApprovalFuture<Option<ApprovalDecision>> {
        Box::pin(async { Ok(Some(ApprovalDecision::allow())) })
    }
}

#[derive(Clone)]
struct FullApprovalRequestProvider {
    requests: Arc<Mutex<Vec<ApprovalRequest>>>,
}

impl ApprovalProvider for FullApprovalRequestProvider {
    fn should_request(&self, request: &ApprovalRequest) -> bool {
        self.requests
            .lock()
            .expect("full approval requests")
            .push(request.clone());
        true
    }

    fn decide(&self, _request: &ApprovalRequest) -> ApprovalFuture<Option<ApprovalDecision>> {
        Box::pin(async { Ok(Some(ApprovalDecision::allow())) })
    }
}

#[test]
fn configured_child_approval_uses_canonical_identity_auto_broker_and_includes_finish() {
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = build_default_registry();
    register_counting_tool(
        &mut registry,
        "approval_action",
        false,
        executions.clone(),
        None,
    );
    let llm = ScriptedLlmClient::from_steps(vec![
        ScriptStep::response(delegate_response()),
        ScriptStep::response(LLMResponse::with_tool_calls(
            "",
            vec![ToolCall::from_raw_arguments(
                "child-action",
                "approval_action",
                json!({"scope": "child"}),
            )],
        )),
        ScriptStep::response(finish_response("child-finish", "child done")),
        ScriptStep::response(finish_response("parent-finish", "parent done")),
    ]);
    let requests = Arc::new(Mutex::new(Vec::<ApprovalRequest>::new()));
    let provider = FullApprovalRequestProvider {
        requests: requests.clone(),
    };
    let lifecycle = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
    let lifecycle_for_handler = lifecycle.clone();
    let event_handler: vv_agent::RuntimeEventHandler = Arc::new(move |name, payload| {
        if name == "sub_run_started" {
            lifecycle_for_handler
                .lock()
                .expect("approval lifecycle")
                .push((
                    name.to_string(),
                    serde_json::to_value(payload).expect("payload"),
                ));
        }
    });
    let mut parent = configured_parent(&["approval_action"], &[]);
    let child = parent
        .sub_agents
        .get_mut(CHILD_AGENT)
        .expect("configured policy child");
    child
        .metadata
        .insert("agent_name".to_string(), json!("spoofed-child-agent"));
    let mut runtime = AgentRuntime::new(llm).with_tool_registry(registry);
    runtime.set_tool_policy(
        ToolPolicy {
            approval: ApprovalPolicy::Always,
            ..ToolPolicy::default()
        }
        .allow_only([
            "task_finish",
            "create_sub_task",
            "sub_task_status",
            "approval_action",
        ]),
    );

    let result = runtime
        .run_with_controls(
            parent,
            RuntimeRunControls {
                log_handler: Some(event_handler),
                execution_context: Some(ExecutionContext {
                    approval_provider: Some(Arc::new(provider)),
                    metadata: std::collections::BTreeMap::from([(
                        "_vv_agent_trace_id".to_string(),
                        json!("trace-child-approval"),
                    )]),
                    ..ExecutionContext::default()
                }),
                run_context: Some(vv_agent::RunContext {
                    run_id: "parent-approval-run".to_string(),
                    agent_name: "parent".to_string(),
                    ..vv_agent::RunContext::default()
                }),
                ..RuntimeRunControls::default()
            },
        )
        .expect("configured child approval run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    let lifecycle = lifecycle.lock().expect("approval lifecycle");
    let started = &lifecycle[0].1;
    let child_run_id = started["child_run_id"]
        .as_str()
        .expect("approval child run id");
    let child_session_id = started["child_session_id"]
        .as_str()
        .expect("approval child session id");
    let requests = requests.lock().expect("full approval requests");
    let child_requests = requests
        .iter()
        .filter(|request| request.run_id == child_run_id)
        .collect::<Vec<_>>();
    assert_eq!(
        child_requests
            .iter()
            .map(|request| request.tool_name.as_str())
            .collect::<Vec<_>>(),
        ["approval_action", "task_finish"]
    );
    for request in child_requests {
        assert_eq!(request.trace_id, "trace-child-approval");
        assert_eq!(request.agent_name, CHILD_AGENT);
        assert_ne!(request.agent_name, "spoofed-child-agent");
        assert!(request.metadata["tool_metadata"].is_object());
        if request.tool_name == "approval_action" {
            assert_eq!(
                request.metadata["tool_metadata"]["configured_tool"],
                json!("approval_action")
            );
        }
        assert_eq!(request.metadata["session_id"], json!(child_session_id));
    }
}

#[test]
fn configured_child_can_use_tool_denial_precedes_approval_and_executor() {
    let contract = policy_contract();
    let execution_order = Arc::new(Mutex::new(Vec::new()));
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = build_default_registry();
    register_counting_tool(
        &mut registry,
        "guarded_action",
        true,
        executions.clone(),
        Some(execution_order.clone()),
    );

    let captured_child_schemas = Arc::new(Mutex::new(Vec::new()));
    let schemas_for_child = captured_child_schemas.clone();
    let llm = ScriptedLlmClient::from_steps(vec![
        ScriptStep::response(delegate_response()),
        ScriptStep::callback(move |request| {
            *schemas_for_child.lock().expect("child schemas") = schema_names(request);
            Ok(LLMResponse::with_tool_calls(
                "force argument-denied tool",
                vec![ToolCall::from_raw_arguments(
                    "guarded-call",
                    "guarded_action",
                    json!({"scope": "denied"}),
                )],
            ))
        }),
        ScriptStep::response(finish_response("child-finish", "child done")),
        ScriptStep::response(finish_response("parent-finish", "parent done")),
    ]);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = RecordingApprovalProvider {
        requests: requests.clone(),
        execution_order: Some(execution_order.clone()),
    };
    let order_for_predicate = execution_order.clone();
    let mut runtime = AgentRuntime::new(llm).with_tool_registry(registry);
    runtime.set_tool_policy(
        ToolPolicy {
            approval: ApprovalPolicy::OnRequest,
            ..ToolPolicy::default()
        }
        .allow_only(["task_finish", "create_sub_task", "guarded_action"])
        .can_use_tool(move |name, _arguments| {
            if name == "guarded_action" {
                order_for_predicate
                    .lock()
                    .expect("execution order")
                    .push("can_use_tool".to_string());
                return false;
            }
            true
        }),
    );
    let controls = RuntimeRunControls {
        execution_context: Some(ExecutionContext {
            approval_provider: Some(Arc::new(provider)),
            approval_broker: Some(ApprovalBroker::default()),
            ..ExecutionContext::default()
        }),
        ..RuntimeRunControls::default()
    };

    let result = runtime
        .run_with_controls(configured_parent(&["guarded_action"], &[]), controls)
        .expect("configured child run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert!(captured_child_schemas
        .lock()
        .expect("child schemas")
        .iter()
        .any(|name| name == "guarded_action"));
    assert_eq!(
        execution_order.lock().expect("execution order").as_slice(),
        ["can_use_tool"]
    );
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert!(!requests
        .lock()
        .expect("approval requests")
        .iter()
        .any(|name| name == "guarded_action"));

    let fixture_order = contract["execution_order"]
        .as_array()
        .expect("policy execution order")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let can_use_index = fixture_order
        .iter()
        .position(|stage| *stage == "can_use_tool")
        .expect("can_use_tool stage");
    let approval_index = fixture_order
        .iter()
        .position(|stage| *stage == "approval")
        .expect("approval stage");
    let executor_index = fixture_order
        .iter()
        .position(|stage| *stage == "executor")
        .expect("executor stage");
    assert!(can_use_index < approval_index && approval_index < executor_index);
}

#[test]
fn configured_child_required_approval_without_provider_does_not_execute() {
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = build_default_registry();
    register_counting_tool(
        &mut registry,
        "approval_action",
        true,
        executions.clone(),
        None,
    );
    let llm = ScriptedLlmClient::from_steps(vec![
        ScriptStep::response(delegate_response()),
        ScriptStep::response(LLMResponse::with_tool_calls(
            "force approval-required tool",
            vec![ToolCall::from_raw_arguments(
                "approval-call",
                "approval_action",
                json!({}),
            )],
        )),
        ScriptStep::response(finish_response("parent-finish", "parent done")),
    ]);
    let mut runtime = AgentRuntime::new(llm).with_tool_registry(registry);
    runtime.set_tool_policy(
        ToolPolicy {
            approval: ApprovalPolicy::OnRequest,
            ..ToolPolicy::default()
        }
        .allow_only(["task_finish", "create_sub_task", "approval_action"]),
    );

    let result = runtime
        .run(configured_parent(&["approval_action"], &[]))
        .expect("configured child approval run");

    assert_eq!(result.status, AgentStatus::Completed);
    assert_eq!(executions.load(Ordering::SeqCst), 0);
}

fn run_child_approval_mode(
    approval: ApprovalPolicy,
    tool_needs_approval: bool,
    panic_predicate: bool,
) -> (Vec<String>, usize, usize) {
    let executions = Arc::new(AtomicUsize::new(0));
    let predicate_calls = Arc::new(AtomicUsize::new(0));
    let mut registry = build_default_registry();
    let builder = FunctionTool::builder("approval_action")
        .metadata("configured_tool", json!("approval_action"));
    let builder = if panic_predicate {
        let predicate_calls = predicate_calls.clone();
        builder.needs_approval_if(move |_context, _arguments| {
            predicate_calls.fetch_add(1, Ordering::SeqCst);
            panic!("{approval:?} must bypass the dynamic approval predicate")
        })
    } else {
        builder.needs_approval(tool_needs_approval)
    };
    let executions_for_tool = executions.clone();
    let tool = builder
        .handler(move |_context, _arguments: Value| {
            let executions = executions_for_tool.clone();
            async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text("executed"))
            }
        })
        .build()
        .expect("approval mode tool");
    registry
        .register(tool.as_tool_spec())
        .expect("register approval mode tool");
    let llm = ScriptedLlmClient::from_steps(vec![
        ScriptStep::response(delegate_response()),
        ScriptStep::response(LLMResponse::with_tool_calls(
            "force approval-mode tool",
            vec![ToolCall::from_raw_arguments(
                "approval-call",
                "approval_action",
                json!({}),
            )],
        )),
        ScriptStep::response(finish_response("child-finish", "child done")),
        ScriptStep::response(finish_response("parent-finish", "parent done")),
    ]);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = RecordingApprovalProvider {
        requests: requests.clone(),
        execution_order: None,
    };
    let mut runtime = AgentRuntime::new(llm).with_tool_registry(registry);
    runtime.set_tool_policy(
        ToolPolicy {
            approval,
            ..ToolPolicy::default()
        }
        .allow_only(["task_finish", "create_sub_task", "approval_action"]),
    );
    let controls = RuntimeRunControls {
        execution_context: Some(ExecutionContext {
            approval_provider: Some(Arc::new(provider)),
            approval_broker: Some(ApprovalBroker::default()),
            ..ExecutionContext::default()
        }),
        ..RuntimeRunControls::default()
    };

    let result = runtime
        .run_with_controls(configured_parent(&["approval_action"], &[]), controls)
        .expect("configured child approval run");
    assert_eq!(result.status, AgentStatus::Completed);
    let request_names = requests.lock().expect("approval requests").clone();
    (
        request_names,
        executions.load(Ordering::SeqCst),
        predicate_calls.load(Ordering::SeqCst),
    )
}

#[test]
fn configured_child_honors_all_approval_modes_and_predicate_bypass() {
    let modes = &policy_contract()["approval_modes"];
    assert_eq!(
        modes["values"],
        json!(["default", "always", "never", "on_request"])
    );
    assert_eq!(modes["default_is_merge_sentinel"], true);
    assert_eq!(modes["on_request_is_explicit_override"], true);
    assert_eq!(
        modes["tool_declaration_modes"],
        json!(["default", "on_request"])
    );
    assert_eq!(modes["predicate_bypass_modes"], json!(["always", "never"]));
    assert_eq!(ApprovalPolicy::default(), ApprovalPolicy::Default);

    for policy in [ApprovalPolicy::Default, ApprovalPolicy::OnRequest] {
        let (requests, executions, predicate_calls) = run_child_approval_mode(policy, false, false);
        assert!(!requests.iter().any(|name| name == "approval_action"));
        assert_eq!(executions, 1);
        assert_eq!(predicate_calls, 0);

        let (requests, executions, predicate_calls) = run_child_approval_mode(policy, true, false);
        assert_eq!(
            requests
                .iter()
                .filter(|name| name.as_str() == "approval_action")
                .count(),
            1
        );
        assert_eq!(executions, 1);
        assert_eq!(predicate_calls, 0);
    }

    let (always_requests, always_executions, always_predicate_calls) =
        run_child_approval_mode(ApprovalPolicy::Always, false, true);
    assert_eq!(
        always_requests
            .iter()
            .filter(|name| name.as_str() == "approval_action")
            .count(),
        1
    );
    assert_eq!(always_executions, 1);
    assert_eq!(always_predicate_calls, 0);

    let (never_requests, never_executions, never_predicate_calls) =
        run_child_approval_mode(ApprovalPolicy::Never, true, true);
    assert!(!never_requests.iter().any(|name| name == "approval_action"));
    assert_eq!(never_executions, 1);
    assert_eq!(never_predicate_calls, 0);
}
