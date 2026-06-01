use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;
use vv_agent::{
    Agent, AgentStatus, JsonlRunEventStore, LLMResponse, ModelRef, RunConfig, RunEvent,
    RunEventPayload, RunEventReplayQuery, RunEventStore, Runner, ScriptedModelProvider, ToolCall,
};

#[test]
fn jsonl_event_store_appends_and_replays_run_and_children() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("events.jsonl");
    let store = JsonlRunEventStore::new(&path);

    let parent = RunEvent::run_started("run_parent", "trace_1", "parent", "hello");
    let child = RunEvent::run_started("run_child", "trace_1", "child", "sub")
        .with_parent_run_id("run_parent");
    store.append(&parent).expect("append parent");
    store.append(&child).expect("append child");

    let replayed = store
        .replay(RunEventReplayQuery::run("run_parent").include_children(true))
        .expect("replay")
        .collect::<Result<Vec<_>, _>>()
        .expect("events");

    assert_eq!(replayed.len(), 2);
    assert_eq!(replayed[0].run_id(), "run_parent");
    assert_eq!(replayed[1].run_id(), "run_child");
    assert_eq!(replayed[1].parent_run_id(), Some("run_parent"));
}

#[test]
fn jsonl_event_store_replay_can_exclude_children() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = JsonlRunEventStore::new(dir.path().join("events.jsonl"));

    store
        .append(&RunEvent::run_started(
            "run_parent",
            "trace_1",
            "parent",
            "hello",
        ))
        .expect("append parent");
    store
        .append(
            &RunEvent::run_started("run_child", "trace_1", "child", "sub")
                .with_parent_run_id("run_parent"),
        )
        .expect("append child");

    let replayed = store
        .replay(RunEventReplayQuery::run("run_parent"))
        .expect("replay")
        .collect::<Result<Vec<_>, _>>()
        .expect("events");

    assert_eq!(replayed.len(), 1);
    assert_eq!(replayed[0].run_id(), "run_parent");
}

#[tokio::test]
async fn runner_appends_captured_events_to_configured_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(JsonlRunEventStore::new(dir.path().join("events.jsonl")));
    let provider = ScriptedModelProvider::new(
        "scripted",
        "demo-model",
        vec![finish_response("stored final answer")],
    );
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()
        .expect("runner");
    let agent = Agent::builder("store-agent")
        .instructions("Answer directly.")
        .model(ModelRef::named("demo-model"))
        .build()
        .expect("agent");

    let result = runner
        .run_with_config(
            &agent,
            "persist events",
            RunConfig::builder().event_store(store.clone()).build(),
        )
        .await
        .expect("run");

    assert_eq!(result.status(), AgentStatus::Completed);
    let replayed = store
        .replay(RunEventReplayQuery::run("store-agent_run"))
        .expect("replay")
        .collect::<Result<Vec<_>, _>>()
        .expect("events");

    assert!(replayed
        .iter()
        .any(|event| matches!(event.payload(), RunEventPayload::RunStarted { .. })));
    assert!(replayed.iter().any(|event| matches!(
        event.payload(),
        RunEventPayload::RunCompleted {
            status: AgentStatus::Completed
        }
    )));
}

fn finish_response(message: &str) -> LLMResponse {
    let mut args = BTreeMap::new();
    args.insert("message".to_string(), json!(message));
    LLMResponse::with_tool_calls("", vec![ToolCall::new("finish", "task_finish", args)])
}
