use std::sync::{Arc, Mutex};

use serde_json::json;
use vv_agent::{
    LLMResponse, LlmClient, LlmError, LlmRequest, Message, ModelSettings, ScriptStep,
    ScriptedLlmClient,
};

#[test]
fn scripted_llm_accepts_callable_steps() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_for_step = Arc::clone(&seen);
    let llm = ScriptedLlmClient::from_steps(vec![
        ScriptStep::callback(move |request| {
            seen_for_step.lock().expect("seen lock").push(format!(
                "{}:{}:{}:{}:{:?}",
                request.model,
                request.messages[0].content,
                request.tools.len(),
                request.metadata["trace_id"],
                request
                    .model_settings
                    .as_ref()
                    .and_then(|settings| settings.max_tokens)
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
    request.metadata = json!({"trace_id": "trace-1"});
    request.model_settings = Some(ModelSettings::builder().max_tokens(321).build());

    let first = llm.complete(request).expect("dynamic step");
    let second = llm
        .complete(LlmRequest::new("model-b", Vec::new()))
        .expect("static step");

    assert_eq!(first.content, "dynamic response");
    assert_eq!(second.content, "static response");
    assert_eq!(
        seen.lock().expect("seen lock").as_slice(),
        ["model-a:hello:1:\"trace-1\":Some(321)"]
    );
}

#[test]
fn scripted_llm_reports_exhausted_steps() {
    let llm = ScriptedLlmClient::new(Vec::new());
    let error = llm
        .complete(LlmRequest::new("model", Vec::new()))
        .expect_err("empty scripted queue should fail");

    assert!(matches!(error, LlmError::ScriptExhausted));
}
