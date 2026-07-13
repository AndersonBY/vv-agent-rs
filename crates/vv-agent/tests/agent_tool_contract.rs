use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::{
    Agent, LLMResponse, LlmRequest, ModelRef, ModelSettings, RunConfig, Runner, ScriptStep,
    ScriptedModelProvider, Tool, ToolCall,
};

fn finish(message: &str) -> LLMResponse {
    LLMResponse::with_tool_calls(
        "",
        vec![ToolCall::from_raw_arguments(
            format!("finish-{message}"),
            "task_finish",
            json!({"message": message}),
        )],
    )
}

#[tokio::test]
async fn agent_tool_schema_and_child_resolution_match_shared_contract() {
    let captured = Arc::new(Mutex::new(Vec::<LlmRequest>::new()));
    let requests = captured.clone();
    let responses = [
        LLMResponse::with_tool_calls(
            "delegate",
            vec![ToolCall::from_raw_arguments(
                "research",
                "research",
                json!({
                    "task_description": "find facts",
                    "output_requirements": "Return three bullets.",
                    "include_main_summary": true,
                }),
            )],
        ),
        finish("child facts"),
        finish("parent final"),
    ];
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "provider-model",
        responses
            .into_iter()
            .map(|response| {
                let requests = requests.clone();
                ScriptStep::callback(move |request| {
                    requests.lock().expect("requests").push(request.clone());
                    Ok(response.clone())
                })
            })
            .collect(),
    );
    let child = Agent::builder("researcher")
        .instructions("Research.")
        .model(ModelRef::named("child-model"))
        .model_settings(ModelSettings::builder().temperature(0.7).build())
        .build()
        .expect("child");
    let tool = child
        .as_tool()
        .name("research")
        .description("Research facts.")
        .build()
        .expect("agent tool");
    let schema = tool.parameters_schema().clone();
    let parent = Agent::builder("writer")
        .instructions("Delegate research.")
        .model(ModelRef::named("parent-model"))
        .model_settings(ModelSettings::builder().temperature(0.1).build())
        .tool(tool)
        .build()
        .expect("parent");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(".")
        .build()
        .expect("runner");

    let result = runner
        .run_with_config(
            &parent,
            "write report",
            RunConfig::builder()
                .model_settings(ModelSettings::builder().temperature(0.2).build())
                .build(),
        )
        .await
        .expect("run");

    assert_eq!(
        schema,
        json!({
            "type": "object",
            "properties": {
                "task_description": {
                    "type": "string",
                    "description": "Task for the delegated agent."
                },
                "output_requirements": {
                    "type": "string",
                    "description": "Optional output requirements for the delegated agent."
                },
                "include_main_summary": {
                    "type": "boolean",
                    "description": "Whether to include parent task summary."
                }
            },
            "required": ["task_description"],
            "additionalProperties": false
        })
    );
    assert_eq!(result.final_output(), Some("parent final"));
    let requests = captured.lock().expect("requests");
    assert_eq!(
        requests
            .iter()
            .map(|request| request.model.as_str())
            .collect::<Vec<_>>(),
        ["parent-model", "child-model", "parent-model"]
    );
    assert_eq!(
        requests
            .iter()
            .map(|request| {
                request
                    .model_settings
                    .as_ref()
                    .and_then(|settings| settings.temperature)
            })
            .collect::<Vec<_>>(),
        [Some(0.2), Some(0.7), Some(0.2)]
    );
    let child_prompt = requests[1]
        .messages
        .iter()
        .find(|message| message.role == vv_agent::MessageRole::User)
        .expect("child user prompt")
        .content
        .as_str();
    assert!(child_prompt
        .contains("<Output Requirements>\nReturn three bullets.\n</Output Requirements>"));
    assert!(child_prompt.contains("<Main Task Summary>\nwrite report\n</Main Task Summary>"));
}
