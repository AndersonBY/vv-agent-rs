use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::{
    LLMResponse, LlmClient, LlmError, LlmRequest, Message, ScriptStep, ScriptedLlmClient,
};

#[test]
fn scripted_llm_accepts_callable_steps_like_python() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_for_step = Arc::clone(&seen);
    let llm = ScriptedLlmClient::from_steps(vec![
        ScriptStep::callback(move |request| {
            seen_for_step.lock().expect("seen lock").push(format!(
                "{}:{}:{}",
                request.model,
                request.messages[0].content,
                request.tools.len()
            ));
            Ok(LLMResponse::new("dynamic response"))
        }),
        ScriptStep::response(LLMResponse::new("static response")),
    ]);

    let mut request = LlmRequest::new("model-a", vec![Message::user("hello")]);
    request.tools.push(json!({
        "type": "function",
        "function": {"name": "task_finish", "parameters": {"type": "object"}}
    }));

    let first = llm.complete(request).expect("dynamic step");
    let second = llm
        .complete(LlmRequest::new("model-b", Vec::new()))
        .expect("static step");

    assert_eq!(first.content, "dynamic response");
    assert_eq!(second.content, "static response");
    assert_eq!(
        seen.lock().expect("seen lock").as_slice(),
        ["model-a:hello:1"]
    );
}

#[test]
fn scripted_llm_reports_exhausted_steps_like_python() {
    let llm = ScriptedLlmClient::new(Vec::new());
    let error = llm
        .complete(LlmRequest::new("model", Vec::new()))
        .expect_err("empty scripted queue should fail");

    assert!(matches!(error, LlmError::ScriptExhausted));
}
