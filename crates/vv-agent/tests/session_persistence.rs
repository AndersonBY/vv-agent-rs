use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::{
    session_store_conformance, Agent, LLMResponse, LlmRequest, MemorySession, MemorySessionStore,
    MessageRole, ModelRef, RedisSessionStore, RunConfig, Runner, ScriptStep, ScriptedModelProvider,
    Session, SessionItem, SqliteSessionStore, ToolCall,
};

const SESSION_ITEMS_FIXTURE: &str = include_str!("fixtures/parity/session_items.jsonl");
const RUNNER_SESSION_FIXTURE: &str = include_str!("fixtures/parity/runner_session_messages.jsonl");

#[test]
fn session_item_wire_matches_python_message_fixture() {
    for line in SESSION_ITEMS_FIXTURE.lines() {
        let expected: serde_json::Value = serde_json::from_str(line).expect("fixture json");
        let item: SessionItem = serde_json::from_str(line).expect("canonical session item");

        assert_eq!(item.to_message().to_dict(), expected);
        assert_eq!(
            serde_json::to_string(&item).expect("serialize session item"),
            line
        );
    }
}

#[tokio::test]
async fn memory_session_store_passes_conformance() {
    session_store_conformance(&MemorySessionStore::new())
        .await
        .expect("memory session conformance");
}

#[tokio::test]
async fn sqlite_session_store_passes_conformance() {
    let store = SqliteSessionStore::open_memory().expect("sqlite session store");
    session_store_conformance(&store)
        .await
        .expect("sqlite session conformance");
}

#[tokio::test]
async fn redis_session_store_passes_conformance_when_configured() {
    let Ok(redis_url) = std::env::var("VV_AGENT_TEST_REDIS_URL") else {
        return;
    };
    let key_prefix = format!("vv-agent-session-test-{}", std::process::id());
    let store =
        RedisSessionStore::with_key_prefix(redis_url, key_prefix).expect("redis session store");

    session_store_conformance(&store)
        .await
        .expect("redis session conformance");
}

#[tokio::test]
async fn runner_persists_complete_result_message_delta_into_next_provider_history() {
    let requests = Arc::new(Mutex::new(Vec::<LlmRequest>::new()));
    let first_requests = requests.clone();
    let second_requests = requests.clone();
    let provider = ScriptedModelProvider::from_steps(
        "scripted",
        "history-model",
        vec![
            ScriptStep::callback(move |request| {
                first_requests
                    .lock()
                    .expect("requests")
                    .push(request.clone());
                Ok(finish_response(
                    "first assistant",
                    "finish_1",
                    "first result",
                ))
            }),
            ScriptStep::callback(move |request| {
                second_requests
                    .lock()
                    .expect("requests")
                    .push(request.clone());
                Ok(finish_response(
                    "second assistant",
                    "finish_2",
                    "second result",
                ))
            }),
        ],
    );
    let workspace = tempfile::tempdir().expect("workspace");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace(workspace.path())
        .build()
        .expect("runner");
    let agent = Agent::builder("history-agent")
        .instructions("Preserve exact conversation history.")
        .model(ModelRef::named("history-model"))
        .build()
        .expect("agent");
    let session = MemorySession::new("runner-history");

    let first = runner
        .run_with_config(
            &agent,
            "first input",
            RunConfig::builder().session(session.clone()).build(),
        )
        .await
        .expect("first run");
    let first_items = session.get_items(None).await.expect("first session items");
    let first_messages = first_items
        .iter()
        .map(SessionItem::to_message)
        .collect::<Vec<_>>();
    assert_eq!(first_messages, first.result().messages[1..]);

    runner
        .run_with_config(
            &agent,
            "second input",
            RunConfig::builder().session(session.clone()).build(),
        )
        .await
        .expect("second run");

    let items = session.get_items(None).await.expect("session items");
    let actual = items
        .iter()
        .map(|item| serde_json::to_value(item).expect("serialize session item"))
        .collect::<Vec<_>>();
    let expected = RUNNER_SESSION_FIXTURE
        .lines()
        .map(|line| serde_json::from_str(line).expect("runner session fixture"))
        .collect::<Vec<serde_json::Value>>();
    assert_eq!(actual, expected);

    let requests = requests.lock().expect("requests");
    assert_eq!(requests.len(), 2);
    let second_history = &requests[1].messages;
    assert_eq!(second_history[0].role, MessageRole::System);
    assert_eq!(second_history[1..4], first_messages);
    assert_eq!(second_history[4].role, MessageRole::User);
    assert_eq!(second_history[4].content, "second input");
    assert_eq!(second_history[2].tool_calls[0].id, "finish_1");
    assert_eq!(second_history[3].tool_call_id.as_deref(), Some("finish_1"));
}

fn finish_response(content: &str, call_id: &str, final_message: &str) -> LLMResponse {
    LLMResponse::with_tool_calls(
        content,
        vec![ToolCall::new(
            call_id,
            "task_finish",
            BTreeMap::from([("message".to_string(), json!(final_message))]),
        )],
    )
}
