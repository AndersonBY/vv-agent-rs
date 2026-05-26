use std::sync::{Arc, Mutex};

use vv_agent::{AgentDefinition, AgentRun, AgentSession, AgentStatus, ResolvedModelConfig};

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
