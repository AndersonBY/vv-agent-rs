use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use vv_agent::{
    AgentDefinition, AgentRun, AgentSession, AgentStatus, ResolvedModelConfig, SessionEventHandler,
};

#[test]
fn session_prompt_supports_follow_up_queue() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let execute_run = {
        let calls = Arc::clone(&calls);
        Arc::new(move |prompt: String| {
            calls.lock().expect("calls").push(prompt.clone());
            Ok(fake_run(&prompt, AgentStatus::Completed))
        })
    };
    let mut session = AgentSession::new(
        execute_run,
        "demo",
        AgentDefinition::default_for_model("demo"),
        "./workspace",
    );

    session.follow_up("after first run").expect("follow_up");
    let run = session.prompt("first run").expect("prompt");

    assert_eq!(run.result.status, AgentStatus::Completed);
    assert_eq!(
        *calls.lock().expect("calls"),
        vec!["first run".to_string(), "after first run".to_string()]
    );
    assert_eq!(
        session
            .state()
            .latest_run
            .unwrap()
            .result
            .final_answer
            .as_deref(),
        Some("after first run")
    );
}

#[test]
fn session_continue_run_uses_queued_prompt_without_auto_follow_up() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let execute_run = {
        let calls = Arc::clone(&calls);
        Arc::new(move |prompt: String| {
            calls.lock().expect("calls").push(prompt.clone());
            Ok(fake_run(&prompt, AgentStatus::Completed))
        })
    };
    let mut session = AgentSession::new(
        execute_run,
        "demo",
        AgentDefinition::default_for_model("demo"),
        "./workspace",
    );

    session.follow_up("queued follow-up").expect("follow_up");
    let run = session.continue_run(None).expect("continue");

    assert_eq!(run.result.final_answer.as_deref(), Some("queued follow-up"));
    assert_eq!(
        *calls.lock().expect("calls"),
        vec!["queued follow-up".to_string()]
    );
}

#[test]
fn session_query_raises_when_not_completed() {
    let execute_run = Arc::new(move |prompt: String| Ok(fake_run(&prompt, AgentStatus::WaitUser)));
    let mut session = AgentSession::new(
        execute_run,
        "demo",
        AgentDefinition::default_for_model("demo"),
        "./workspace",
    );

    let error = session.query("ask").expect_err("query error");

    assert!(error.contains("status=wait_user"));
}

#[test]
fn session_emits_queue_and_run_events() {
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let execute_run = {
        let calls = Arc::clone(&calls);
        Arc::new(move |prompt: String| {
            calls.lock().expect("calls").push(prompt.clone());
            Ok(fake_run(&prompt, AgentStatus::Completed))
        })
    };
    let mut session = AgentSession::new(
        execute_run,
        "demo",
        AgentDefinition::default_for_model("demo"),
        "./workspace",
    );
    let events = recorded_events();
    session.subscribe(recording_listener(&events));

    session.follow_up("after first").expect("follow_up");
    let run = session.prompt("first").expect("prompt");

    assert_eq!(run.result.final_answer.as_deref(), Some("after first"));
    let events = events.lock().expect("events");
    let event_names: Vec<&str> = events.iter().map(|(event, _)| event.as_str()).collect();
    assert_eq!(
        event_names,
        vec![
            "session_follow_up_queued",
            "session_run_start",
            "session_run_end",
            "session_follow_up_dequeued",
            "session_run_start",
            "session_run_end",
        ]
    );
    assert_eq!(events[1].1["prompt"], Value::String("first".to_string()));
    assert_eq!(events[1].1["existing_messages"], Value::from(0));
    assert_eq!(
        events[2].1["status"],
        Value::String("completed".to_string())
    );
    assert_eq!(
        events[3].1["prompt"],
        Value::String("after first".to_string())
    );
}

#[test]
fn session_unsubscribe_removes_listener() {
    let execute_run = Arc::new(move |prompt: String| Ok(fake_run(&prompt, AgentStatus::Completed)));
    let mut session = AgentSession::new(
        execute_run,
        "demo",
        AgentDefinition::default_for_model("demo"),
        "./workspace",
    );
    let events = recorded_events();
    let listener_id = session.subscribe(recording_listener(&events));

    assert!(session.unsubscribe(listener_id));
    session.follow_up("silent").expect("follow_up");

    assert!(events.lock().expect("events").is_empty());
}

#[test]
fn session_clear_queues_emits_event_and_drops_prompts() {
    let execute_run = Arc::new(move |prompt: String| Ok(fake_run(&prompt, AgentStatus::Completed)));
    let mut session = AgentSession::new(
        execute_run,
        "demo",
        AgentDefinition::default_for_model("demo"),
        "./workspace",
    );
    let events = recorded_events();
    session.subscribe(recording_listener(&events));

    session.steer("urgent").expect("steer");
    session.follow_up("later").expect("follow_up");
    session.clear_queues();
    let error = session.continue_run(None).expect_err("empty queue");

    assert!(error.contains("No queued prompt available"));
    let events = events.lock().expect("events");
    let event_names: Vec<&str> = events.iter().map(|(event, _)| event.as_str()).collect();
    assert_eq!(
        event_names,
        vec![
            "session_steer_queued",
            "session_follow_up_queued",
            "session_queues_cleared",
        ]
    );
}

fn fake_run(prompt: &str, status: AgentStatus) -> AgentRun {
    let mut result = vv_agent::AgentResult::completed(vec![], vec![], prompt.to_string());
    result.status = status;
    if status == AgentStatus::WaitUser {
        result.wait_reason = Some("need input".to_string());
        result.final_answer = None;
    }
    AgentRun {
        agent_name: "demo".to_string(),
        result,
        resolved: ResolvedModelConfig::new("demo", "demo", "demo", "demo", vec![]),
    }
}

type RecordedEvents = Arc<Mutex<Vec<(String, BTreeMap<String, Value>)>>>;

fn recorded_events() -> RecordedEvents {
    Arc::new(Mutex::new(Vec::new()))
}

fn recording_listener(events: &RecordedEvents) -> SessionEventHandler {
    let events = Arc::clone(events);
    Arc::new(move |event, payload| {
        events
            .lock()
            .expect("events")
            .push((event.to_string(), payload.clone()));
    })
}
