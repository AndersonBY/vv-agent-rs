use serde::Deserialize;
use serde_json::json;
use vv_agent::{Agent, LLMResponse, ModelRef, Runner, ScriptedModelProvider, ToolCall};

#[derive(Debug, Deserialize)]
struct ResearchSummary {
    answer: String,
    sources: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let final_output = serde_json::to_string(&json!({
        "answer": "The workspace uses a Rust SDK facade.",
        "sources": ["README.md", "docs/architecture.md"]
    }))?;
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "demo-model",
            vec![LLMResponse::with_tool_calls(
                "",
                vec![ToolCall::from_raw_arguments(
                    "finish",
                    "task_finish",
                    json!({"message": final_output}),
                )],
            )],
        ))
        .workspace(".")
        .build()?;
    let agent = Agent::builder("researcher")
        .instructions("Return the requested JSON through task_finish.")
        .model(ModelRef::named("demo-model"))
        .output_type::<ResearchSummary>()
        .build()?;

    let result = runner.run(&agent, "Summarize the workspace").await?;
    let output: ResearchSummary = result.deserialize()?;

    println!("{}", output.answer);
    println!("sources: {}", output.sources.join(", "));
    Ok(())
}
