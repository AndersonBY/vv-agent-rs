use vv_agent::{Agent, AgentStatus, LLMResponse, ModelRef, Runner, ScriptedModelProvider};

#[tokio::test]
async fn runner_facade_can_run_a_simple_prompt() {
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "demo",
            vec![LLMResponse::new("final answer")],
        ))
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("demo")
        .instructions("Answer and finish.")
        .model(ModelRef::named("demo"))
        .build()
        .expect("agent");

    let result = runner.run(&agent, "say hello").await.expect("run");

    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(result.final_output(), Some("final answer"));
}
