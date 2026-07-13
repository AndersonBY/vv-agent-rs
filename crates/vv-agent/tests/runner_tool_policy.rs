use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use vv_agent::{
    handoff, Agent, AgentStatus, BeforeLlmEvent, BeforeLlmPatch, FunctionTool, LLMResponse,
    ModelRef, RunConfig, Runner, RuntimeHook, ScriptedModelProvider, Tool, ToolCall, ToolOutput,
    ToolPolicy,
};

#[derive(Default)]
struct TaskCapture {
    extra_tool_names: Mutex<Vec<String>>,
    metadata: Mutex<BTreeMap<String, Value>>,
}

impl TaskCapture {
    fn extra_tool_names(&self) -> Vec<String> {
        self.extra_tool_names
            .lock()
            .expect("extra tool names")
            .clone()
    }

    fn metadata(&self) -> BTreeMap<String, Value> {
        self.metadata.lock().expect("metadata").clone()
    }
}

impl RuntimeHook for TaskCapture {
    fn before_llm(&self, event: BeforeLlmEvent<'_>) -> Option<BeforeLlmPatch> {
        *self.extra_tool_names.lock().expect("extra tool names") =
            event.task.extra_tool_names.clone();
        *self.metadata.lock().expect("metadata") = event.task.metadata.clone();
        None
    }
}

#[tokio::test]
async fn runner_exposes_custom_agent_and_handoff_tool_schemas() {
    let custom_parameters = json!({
        "type": "object",
        "properties": {
            "order_id": {"type": "string"},
            "detail": {"type": "string", "enum": ["summary", "full"]}
        },
        "required": ["order_id", "detail"]
    });
    let custom_tool = FunctionTool::builder("lookup_order")
        .description("Look up an order with the requested detail level.")
        .json_schema(custom_parameters.clone())
        .handler(|_context, _arguments: Value| async {
            Ok(ToolOutput::text("unused custom result"))
        })
        .build()
        .expect("custom tool");
    let researcher = Agent::builder("researcher")
        .instructions("Research delegated tasks.")
        .model(ModelRef::backend("scripted", "child-model"))
        .build()
        .expect("researcher");
    let research_tool = researcher
        .as_tool()
        .name("delegate_research")
        .description("Delegate a research task.")
        .build()
        .expect("agent tool");
    let research_parameters = research_tool.parameters_schema().clone();
    let writer = Agent::builder("writer")
        .instructions("Continue the handed-off task.")
        .model(ModelRef::backend("scripted", "writer-model"))
        .build()
        .expect("writer");
    let writer_handoff = handoff(&writer)
        .name("route_to_writer")
        .description("Route the task to the writer.")
        .build();

    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured_requests = requests.clone();
    let provider =
        ScriptedModelProvider::from_callback("scripted", "parent-model", move |request| {
            captured_requests
                .lock()
                .expect("requests")
                .push(request.clone());
            Ok(finish_response("done"))
        });
    let task_capture = Arc::new(TaskCapture::default());
    let agent = Agent::builder("coordinator")
        .instructions("Coordinate tools.")
        .model(ModelRef::backend("scripted", "parent-model"))
        .tool(custom_tool)
        .tool(research_tool)
        .handoff(writer_handoff)
        .hook(task_capture.clone())
        .build()
        .expect("coordinator");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");

    let result = runner.run(&agent, "coordinate").await.expect("run");

    assert_eq!(result.status(), AgentStatus::Completed);
    let extra_tool_names = task_capture.extra_tool_names();
    assert!(extra_tool_names.contains(&"lookup_order".to_string()));
    assert!(extra_tool_names.contains(&"delegate_research".to_string()));
    assert!(extra_tool_names.contains(&"route_to_writer".to_string()));

    let requests = requests.lock().expect("requests");
    let tools = &requests.first().expect("model request").tools;
    assert_eq!(
        tool_schema(tools, "lookup_order"),
        &json!({
            "type": "function",
            "function": {
                "name": "lookup_order",
                "description": "Look up an order with the requested detail level.",
                "parameters": custom_parameters,
                "strict": true
            }
        })
    );
    assert_eq!(
        tool_schema(tools, "delegate_research"),
        &json!({
            "type": "function",
            "function": {
                "name": "delegate_research",
                "description": "Delegate a research task.",
                "parameters": research_parameters
            }
        })
    );
    assert_eq!(
        tool_schema(tools, "route_to_writer"),
        &json!({
            "type": "function",
            "function": {
                "name": "route_to_writer",
                "description": "Route the task to the writer.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "input": {
                            "type": "string",
                            "description": "Input or handoff summary for the target agent.",
                            "minLength": 1
                        }
                    },
                    "required": ["input"],
                    "additionalProperties": false
                }
            }
        })
    );
}

#[tokio::test]
async fn merged_tool_policy_filters_schemas_and_blocks_forced_calls_with_never_approval() {
    let executed = Arc::new(AtomicUsize::new(0));
    let allowed = counting_tool("allowed_custom", executed.clone());
    let not_allowed = counting_tool("not_allowed", executed.clone());
    let agent_blocked = counting_tool("agent_blocked", executed.clone());
    let runner_blocked = counting_tool("runner_blocked", executed.clone());
    let run_blocked = counting_tool("run_blocked", executed.clone());

    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured_requests = requests.clone();
    let provider =
        ScriptedModelProvider::from_callback("scripted", "policy-model", move |request| {
            captured_requests
                .lock()
                .expect("requests")
                .push(request.clone());
            Ok(LLMResponse::with_tool_calls(
                "forced calls",
                vec![
                    ToolCall::from_raw_arguments("not_allowed_call", "not_allowed", json!({})),
                    ToolCall::from_raw_arguments("agent_call", "agent_blocked", json!({})),
                    ToolCall::from_raw_arguments("runner_call", "runner_blocked", json!({})),
                    ToolCall::from_raw_arguments("run_call", "run_blocked", json!({})),
                ],
            ))
        });
    let agent_policy = ToolPolicy::default()
        .allow_only([
            "task_finish",
            "allowed_custom",
            "agent_blocked",
            "runner_blocked",
            "run_blocked",
        ])
        .disallow("agent_blocked");
    let runner_policy = ToolPolicy::default().disallow("runner_blocked");
    let run_policy = ToolPolicy::default().disallow("run_blocked");
    let task_capture = Arc::new(TaskCapture::default());
    let agent = Agent::builder("policy_agent")
        .instructions("Use permitted tools only.")
        .model(ModelRef::backend("scripted", "policy-model"))
        .tool(allowed)
        .tool(not_allowed)
        .tool(agent_blocked)
        .tool(runner_blocked)
        .tool(run_blocked)
        .tool_policy(agent_policy)
        .hook(task_capture.clone())
        .max_cycles(1)
        .build()
        .expect("agent");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .default_run_config(RunConfig::builder().tool_policy(runner_policy).build())
        .build()
        .expect("runner");

    let result = runner
        .run_with_config(
            &agent,
            "force blocked tools",
            RunConfig::builder().tool_policy(run_policy).build(),
        )
        .await
        .expect("run");

    assert_eq!(executed.load(Ordering::SeqCst), 0);
    let requests = requests.lock().expect("requests");
    let visible_names = requests
        .first()
        .expect("model request")
        .tools
        .iter()
        .filter_map(|schema| schema["function"]["name"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(visible_names, vec!["task_finish", "allowed_custom"]);

    let metadata = task_capture.metadata();
    assert_eq!(
        metadata["_vv_agent_allowed_tools"],
        json!([
            "task_finish",
            "allowed_custom",
            "agent_blocked",
            "runner_blocked",
            "run_blocked"
        ])
    );
    assert_eq!(
        metadata["_vv_agent_disallowed_tools"],
        json!(["agent_blocked", "runner_blocked", "run_blocked"])
    );

    let tool_results = &result.result().cycles[0].tool_results;
    assert_eq!(tool_results.len(), 4);
    assert_eq!(
        tool_results[0].error_code.as_deref(),
        Some("tool_not_allowed")
    );
    for (tool_result, policy_source) in tool_results.iter().zip([
        "allowed_tools",
        "disallowed_tools",
        "disallowed_tools",
        "disallowed_tools",
    ]) {
        assert_eq!(tool_result.error_code.as_deref(), Some("tool_not_allowed"));
        assert_eq!(tool_result.metadata["policy_source"], json!(policy_source));
    }
}

#[derive(Debug)]
struct EnablementState {
    enabled: bool,
}

#[tokio::test]
async fn per_run_enablement_skips_registration_and_model_schema() {
    let executions = Arc::new(AtomicUsize::new(0));
    let static_disabled = FunctionTool::builder("static_disabled")
        .enabled(false)
        .handler({
            let executions = executions.clone();
            move |_context, _arguments: Value| {
                let executions = executions.clone();
                async move {
                    executions.fetch_add(1, Ordering::SeqCst);
                    Ok(ToolOutput::text("unexpected"))
                }
            }
        })
        .build()
        .expect("static disabled tool");
    let predicate_disabled = FunctionTool::builder("predicate_disabled")
        .enabled_if(|context| {
            assert!(!context.run.run_id.is_empty());
            assert_eq!(context.run.agent_name, "enablement_agent");
            !context
                .app_state::<EnablementState>()
                .expect("enablement app state")
                .enabled
        })
        .handler({
            let executions = executions.clone();
            move |_context, _arguments: Value| {
                let executions = executions.clone();
                async move {
                    executions.fetch_add(1, Ordering::SeqCst);
                    Ok(ToolOutput::text("unexpected"))
                }
            }
        })
        .build()
        .expect("predicate disabled tool");
    let predicate_enabled = FunctionTool::builder("predicate_enabled")
        .enabled_if(|context| {
            context
                .app_state::<EnablementState>()
                .is_some_and(|state| state.enabled)
        })
        .handler(|_context, _arguments: Value| async { Ok(ToolOutput::text("enabled")) })
        .build()
        .expect("predicate enabled tool");

    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured_requests = requests.clone();
    let provider =
        ScriptedModelProvider::from_callback("scripted", "enablement-model", move |request| {
            captured_requests
                .lock()
                .expect("requests")
                .push(request.clone());
            Ok(LLMResponse::with_tool_calls(
                "force disabled calls",
                vec![
                    ToolCall::from_raw_arguments("static_call", "static_disabled", json!({})),
                    ToolCall::from_raw_arguments("predicate_call", "predicate_disabled", json!({})),
                ],
            ))
        });
    let agent = Agent::builder("enablement_agent")
        .instructions("Use enabled tools.")
        .model(ModelRef::backend("scripted", "enablement-model"))
        .tool(static_disabled)
        .tool(predicate_disabled)
        .tool(predicate_enabled)
        .max_cycles(1)
        .build()
        .expect("agent");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");

    let result = runner
        .run_with_config(
            &agent,
            "run",
            RunConfig::builder()
                .app_state(EnablementState { enabled: true })
                .build(),
        )
        .await
        .expect("run");

    assert_eq!(executions.load(Ordering::SeqCst), 0);
    let requests = requests.lock().expect("requests");
    let visible_names = requests[0]
        .tools
        .iter()
        .filter_map(|schema| schema["function"]["name"].as_str())
        .collect::<Vec<_>>();
    assert!(visible_names.contains(&"predicate_enabled"));
    assert!(!visible_names.contains(&"static_disabled"));
    assert!(!visible_names.contains(&"predicate_disabled"));
    assert!(result.result().cycles[0]
        .tool_results
        .iter()
        .all(|tool_result| {
            tool_result.error_code.as_deref() == Some("tool_not_allowed")
                && tool_result.metadata["policy_source"] == json!("planned_name")
        }));
}

#[tokio::test]
async fn never_approval_still_executes_tools_allowed_by_policy() {
    let executed = Arc::new(AtomicUsize::new(0));
    let allowed = counting_tool("allowed_custom", executed.clone());
    let provider = ScriptedModelProvider::new(
        "scripted",
        "policy-model",
        vec![
            LLMResponse::with_tool_calls(
                "call allowed tool",
                vec![ToolCall::from_raw_arguments(
                    "allowed_call",
                    "allowed_custom",
                    json!({}),
                )],
            ),
            finish_response("done"),
        ],
    );
    let agent = Agent::builder("policy_agent")
        .instructions("Use the allowed tool.")
        .model(ModelRef::backend("scripted", "policy-model"))
        .tool(allowed)
        .tool_policy(ToolPolicy::default().allow_only(["task_finish", "allowed_custom"]))
        .build()
        .expect("agent");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");

    let result = runner.run(&agent, "run allowed tool").await.expect("run");

    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(result.final_output(), Some("done"));
    assert_eq!(executed.load(Ordering::SeqCst), 1);
}

fn counting_tool(name: &str, executed: Arc<AtomicUsize>) -> FunctionTool<Value> {
    FunctionTool::builder(name)
        .description(format!("Run {name}."))
        .handler(move |_context, _arguments: Value| {
            let executed = executed.clone();
            async move {
                executed.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text("executed"))
            }
        })
        .build()
        .expect("counting tool")
}

fn tool_schema<'a>(tools: &'a [Value], name: &str) -> &'a Value {
    tools
        .iter()
        .find(|schema| schema["function"]["name"] == name)
        .unwrap_or_else(|| panic!("missing tool schema: {name}"))
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
