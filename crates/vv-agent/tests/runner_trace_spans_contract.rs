use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use vv_agent::{
    Agent, AgentStatus, LLMResponse, ModelError, ModelProvider, ModelRef, ResolvedModelConfig,
    RunConfig, Runner, ScriptedModelProvider, Span, ToolCall, TraceSink,
};

const FIXTURE: &str = include_str!("fixtures/parity/runner_trace_spans.json");

fn contract() -> Value {
    serde_json::from_str(FIXTURE).expect("trace contract")
}

#[derive(Clone, Default)]
struct CapturingSink {
    records: Arc<Mutex<Vec<(String, Span)>>>,
}

impl TraceSink for CapturingSink {
    fn on_span_start(&self, span: &Span) {
        self.records
            .lock()
            .expect("records")
            .push(("start".to_string(), span.clone()));
    }

    fn on_span_end(&self, span: &Span) {
        self.records
            .lock()
            .expect("records")
            .push(("end".to_string(), span.clone()));
    }
}

#[tokio::test]
async fn public_runner_emits_run_agent_tool_topology() {
    let sink = CapturingSink::default();
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "trace-model",
            vec![finish_response("done")],
        ))
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("trace-agent")
        .instructions("Finish.")
        .model(ModelRef::named("trace-model"))
        .build()
        .expect("agent");
    let result = runner
        .run_with_config(
            &agent,
            "trace",
            RunConfig::builder()
                .trace_sink(Arc::new(sink.clone()))
                .trace_id("trace-public-run")
                .workflow_name("public-workflow")
                .build(),
        )
        .await
        .expect("run");
    let records = sink.records.lock().expect("records").clone();
    let starts = records
        .iter()
        .filter(|(operation, _)| operation == "start")
        .map(|(_, span)| span.name.as_str())
        .collect::<Vec<_>>();
    let ends = records
        .iter()
        .filter(|(operation, _)| operation == "end")
        .map(|(_, span)| span.name.as_str())
        .collect::<Vec<_>>();
    let run = records
        .iter()
        .find(|(operation, span)| operation == "end" && span.name == "run")
        .map(|(_, span)| span)
        .expect("run span");
    let agent_span = records
        .iter()
        .find(|(operation, span)| operation == "end" && span.name == "agent")
        .map(|(_, span)| span)
        .expect("agent span");
    let tool = records
        .iter()
        .find(|(operation, span)| operation == "end" && span.name == "tool")
        .map(|(_, span)| span)
        .expect("tool span");

    assert_eq!(starts, ["run", "agent", "tool"]);
    assert_eq!(ends, ["tool", "agent", "run"]);
    assert_eq!(agent_span.parent_id.as_deref(), Some(run.span_id.as_str()));
    assert_eq!(tool.parent_id.as_deref(), Some(agent_span.span_id.as_str()));
    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(result.trace_id(), "trace-public-run");
    assert_eq!(run.metadata["workflow_name"], "public-workflow");
    assert_eq!(
        result.metadata()["run_span"],
        serde_json::to_value(run).expect("run span projection")
    );
    assert_eq!(contract()["topology"]["parents"]["tool"], "agent");
}

#[tokio::test]
async fn per_run_trace_identity_and_workflow_override_runner_defaults() {
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "trace-model",
            vec![finish_response("default"), finish_response("override")],
        ))
        .workspace("./workspace")
        .default_run_config(
            RunConfig::builder()
                .trace_id("trace-default")
                .workflow_name("workflow-default")
                .build(),
        )
        .build()
        .expect("runner");
    let agent = Agent::builder("trace-agent")
        .instructions("Finish.")
        .model(ModelRef::named("trace-model"))
        .build()
        .expect("agent");

    let default_result = runner.run(&agent, "default").await.expect("default run");
    let override_result = runner
        .run_with_config(
            &agent,
            "override",
            RunConfig::builder()
                .trace_id("trace-override")
                .workflow_name("workflow-override")
                .build(),
        )
        .await
        .expect("override run");

    assert_eq!(default_result.trace_id(), "trace-default");
    assert_eq!(
        default_result.metadata()["run_span"]["metadata"]["workflow_name"],
        "workflow-default"
    );
    assert_eq!(override_result.trace_id(), "trace-override");
    assert_eq!(
        override_result.metadata()["run_span"]["metadata"]["workflow_name"],
        "workflow-override"
    );
}

#[derive(Clone)]
struct FailingProvider;

impl ModelProvider for FailingProvider {
    fn resolve(&self, _model: &ModelRef) -> Result<ResolvedModelConfig, ModelError> {
        Err(ModelError::Config("provider unavailable".to_string()))
    }

    fn client(
        &self,
        _resolved: &ResolvedModelConfig,
    ) -> Result<Arc<dyn vv_agent::LlmClient>, ModelError> {
        unreachable!()
    }
}

#[tokio::test]
async fn provider_failure_closes_agent_and_run_spans() {
    let sink = CapturingSink::default();
    let runner = Runner::builder()
        .model_provider(FailingProvider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("trace-agent")
        .instructions("Finish.")
        .model(ModelRef::named("trace-model"))
        .build()
        .expect("agent");
    let error = match runner
        .run_with_config(
            &agent,
            "trace",
            RunConfig::builder()
                .trace_sink(Arc::new(sink.clone()))
                .build(),
        )
        .await
    {
        Ok(_) => panic!("provider failure must fail"),
        Err(error) => error,
    };
    let records = sink.records.lock().expect("records").clone();
    let ends = records
        .iter()
        .filter(|(operation, _)| operation == "end")
        .map(|(_, span)| span.name.as_str())
        .collect::<Vec<_>>();

    assert!(error.contains("provider unavailable"));
    assert_eq!(ends, ["agent", "run"]);
    assert_eq!(
        records.last().expect("run end").1.metadata["status"],
        "failed"
    );
}

struct BrokenSink;

impl TraceSink for BrokenSink {
    fn on_span_start(&self, _span: &Span) {
        panic!("start down");
    }

    fn on_span_end(&self, _span: &Span) {
        panic!("end down");
    }

    fn flush(&self) -> Result<(), String> {
        Err("flush down".to_string())
    }
}

#[tokio::test]
async fn trace_sink_failures_are_isolated_from_run() {
    let runner = Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "trace-model",
            vec![finish_response("done")],
        ))
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("trace-agent")
        .instructions("Finish.")
        .model(ModelRef::named("trace-model"))
        .build()
        .expect("agent");
    let result = runner
        .run_with_config(
            &agent,
            "trace",
            RunConfig::builder()
                .trace_sink(Arc::new(BrokenSink))
                .build(),
        )
        .await
        .expect("trace sink failures stay isolated");

    assert_eq!(result.status(), AgentStatus::Completed);
    assert_eq!(contract()["sink_failure"]["isolated"], true);
}

fn finish_response(message: &str) -> LLMResponse {
    LLMResponse::with_tool_calls(
        "",
        vec![ToolCall::new(
            "finish",
            "task_finish",
            BTreeMap::from([("message".to_string(), json!(message))]),
        )],
    )
}
