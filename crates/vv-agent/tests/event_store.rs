use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;
use vv_agent::{
    Agent, AgentStatus, JsonlRunEventStore, LLMResponse, ModelRef, RunConfig, RunEvent,
    RunEventPayload, RunEventReplayQuery, RunEventStore, Runner, ScriptedModelProvider, ToolCall,
};

#[test]
fn jsonl_event_store_replays_direct_children_by_default() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("events.jsonl");
    let store = JsonlRunEventStore::new(&path);

    let parent = RunEvent::run_started("run_parent", "trace_1", "parent", "hello");
    let child = RunEvent::run_started("run_child", "trace_1", "child", "sub")
        .with_parent_run_id("run_parent");
    store.append(&parent).expect("append parent");
    store.append(&child).expect("append child");

    let replayed = store
        .replay(RunEventReplayQuery::run("run_parent"))
        .expect("replay")
        .collect::<Result<Vec<_>, _>>()
        .expect("events");

    assert_eq!(replayed.len(), 2);
    assert_eq!(replayed[0].run_id(), "run_parent");
    assert_eq!(replayed[1].run_id(), "run_child");
    assert_eq!(replayed[1].parent_run_id(), Some("run_parent"));
}

#[test]
fn run_event_serializes_with_the_cross_language_flat_wire_shape() {
    let event = RunEvent::run_started("run_1", "trace_1", "assistant", "hello");

    let payload = serde_json::to_value(&event).expect("serialize event");

    assert_eq!(payload["version"], json!("v1"));
    assert_eq!(payload["type"], json!("run_started"));
    assert_eq!(payload["input"], json!("hello"));
    assert!(payload.get("payload").is_none());
    assert!(payload.get("created_at_ms").is_none());
    assert!(payload["created_at"]
        .as_f64()
        .is_some_and(|value| value > 0.0));
    let event_id = payload["event_id"].as_str().expect("event id");
    assert!(event_id.starts_with("evt_"));
    assert_eq!(event_id.len(), 36);

    let round_trip: RunEvent = serde_json::from_value(payload).expect("deserialize event");
    assert_eq!(round_trip.run_id(), "run_1");
    assert!(matches!(
        round_trip.payload(),
        RunEventPayload::RunStarted { input } if input == "hello"
    ));
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
        .replay(RunEventReplayQuery::run("run_parent").include_children(false))
        .expect("replay")
        .collect::<Result<Vec<_>, _>>()
        .expect("events");

    assert_eq!(replayed.len(), 1);
    assert_eq!(replayed[0].run_id(), "run_parent");
}

#[test]
fn jsonl_event_store_replay_is_lazy_and_stops_at_a_corrupt_line() {
    let store = JsonlRunEventStore::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/parity/event_store_replay.jsonl"
    ));

    let mut replay = store
        .replay(RunEventReplayQuery::run("run_parent"))
        .expect("open replay");

    let first = replay.next().expect("first line").expect("first event");
    assert_eq!(first.event_id().as_str(), "evt_parent");

    let error = replay
        .next()
        .expect("corrupt second line")
        .expect_err("second line must fail");
    assert_eq!(error.code(), "event_store_corrupt_line");
    assert_eq!(error.line_number(), Some(2));
    assert_eq!(error.to_string(), "event store corrupt line 2");

    assert!(replay.next().is_none());
}

#[test]
fn jsonl_event_store_replay_of_a_missing_file_is_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = JsonlRunEventStore::new(dir.path().join("missing.jsonl"));

    let mut replay = store
        .replay(RunEventReplayQuery::run("run_parent"))
        .expect("missing replay");

    assert!(replay.next().is_none());
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
    assert!(result.run_id().starts_with("run_"));
    assert_eq!(result.run_id().len(), 36);
    assert!(result.trace_id().starts_with("trace_"));
    assert_eq!(result.trace_id().len(), 38);
    let replayed = store
        .replay(RunEventReplayQuery::run(result.run_id()))
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
