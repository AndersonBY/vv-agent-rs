use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde_json::{json, Value};
use vv_agent::{
    Agent, AgentStatus, FunctionTool, LLMResponse, ModelRef, RunConfig, Runner,
    ScriptedModelProvider, ToolCall, ToolOutput,
};

#[derive(Debug)]
struct AppState {
    tenant_id: String,
}

#[derive(Debug, Deserialize)]
struct UpdateArguments {
    value: String,
}

#[derive(Debug, Clone, PartialEq)]
struct ObservedCall {
    run_id: String,
    agent_name: String,
    model: Option<ModelRef>,
    tool_call_id: String,
    tool_name: String,
    raw_arguments: Value,
    tenant_id: String,
    initial_seed: Value,
}

#[tokio::test]
async fn function_handler_receives_real_identity_app_state_and_mutable_shared_state() {
    let observed = Arc::new(Mutex::new(None));
    let observed_for_tool = observed.clone();
    let tool = FunctionTool::builder("update_state")
        .json_schema(json!({
            "type": "object",
            "properties": {
                "value": {"type": "string"}
            },
            "required": ["value"]
        }))
        .handler(move |context, arguments: UpdateArguments| {
            let observed = observed_for_tool.clone();
            async move {
                let tenant_id = context
                    .app_state::<AppState>()
                    .expect("app state")
                    .tenant_id
                    .clone();
                let initial_seed = context
                    .shared_state_value("seed")
                    .expect("initial shared-state seed");
                context.set_shared_state_value("updated", json!(arguments.value));
                *observed.lock().expect("observed call") = Some(ObservedCall {
                    run_id: context.run.run_id.clone(),
                    agent_name: context.run.agent_name.clone(),
                    model: context.run.model.clone(),
                    tool_call_id: context.tool_call_id.clone(),
                    tool_name: context.tool_name.clone(),
                    raw_arguments: context.raw_arguments.clone(),
                    tenant_id,
                    initial_seed,
                });
                Ok(ToolOutput::text("state updated"))
            }
        })
        .build()
        .expect("state tool");
    let provider = ScriptedModelProvider::new(
        "scripted",
        "context-model",
        vec![
            LLMResponse::with_tool_calls(
                "update state",
                vec![ToolCall::from_raw_arguments(
                    "update_call_42",
                    "update_state",
                    json!({"value": "persisted"}),
                )],
            ),
            LLMResponse::with_tool_calls(
                "finish",
                vec![ToolCall::from_raw_arguments(
                    "finish_call",
                    "task_finish",
                    json!({"message": "done"}),
                )],
            ),
        ],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("context_agent")
        .instructions("Update state, then finish.")
        .model(ModelRef::backend("scripted", "context-model"))
        .tool(tool)
        .build()
        .expect("agent");

    let result = runner
        .run_with_config(
            &agent,
            "update",
            RunConfig::builder()
                .app_state(AppState {
                    tenant_id: "tenant-7".to_string(),
                })
                .initial_shared_state(BTreeMap::from([("seed".to_string(), json!("initial"))]))
                .build(),
        )
        .await
        .expect("run");

    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(result.final_output(), Some("done"));
    assert_eq!(result.result().shared_state["seed"], json!("initial"));
    assert_eq!(result.result().shared_state["updated"], json!("persisted"));
    assert_eq!(
        observed.lock().expect("observed call").clone(),
        Some(ObservedCall {
            run_id: result.run_id().to_string(),
            agent_name: "context_agent".to_string(),
            model: Some(ModelRef::named("context-model")),
            tool_call_id: "update_call_42".to_string(),
            tool_name: "update_state".to_string(),
            raw_arguments: json!({"value": "persisted"}),
            tenant_id: "tenant-7".to_string(),
            initial_seed: json!("initial"),
        })
    );
}
