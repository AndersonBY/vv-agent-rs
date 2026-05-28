use std::collections::BTreeMap;

use serde_json::{json, Value};
use vv_agent::types::CycleTokenUsage;
use vv_agent::{
    AgentResult, AgentStatus, AgentTask, CycleRecord, LLMResponse, Message, NoToolPolicy,
    SubTaskOutcome, SubTaskRequest, TokenUsage, ToolCall, ToolDirective, ToolExecutionResult,
    ToolResultStatus,
};

#[test]
fn tool_execution_result_dict_matches_python_status_shape() {
    let success = ToolExecutionResult::success("call-1", "ok");
    let success_dict = success.to_dict();
    assert_eq!(success_dict["status"], json!("success"));
    assert_eq!(success_dict["status_code"], json!("SUCCESS"));
    assert_eq!(success_dict["directive"], json!("continue"));

    let mut wait = ToolExecutionResult::success("call-2", "wait");
    wait.status = ToolResultStatus::WaitResponse;
    wait.directive = ToolDirective::WaitUser;
    let wait_dict = wait.to_dict();
    assert_eq!(wait_dict["status"], json!("success"));
    assert_eq!(wait_dict["status_code"], json!("WAIT_RESPONSE"));
    assert_eq!(wait_dict["directive"], json!("wait_user"));

    let error = ToolExecutionResult::error("call-3", "bad");
    let error_dict = error.to_dict();
    assert_eq!(error_dict["status"], json!("error"));
    assert_eq!(error_dict["status_code"], json!("ERROR"));
}

#[test]
fn agent_result_dict_round_trips_python_celery_payload_shape() {
    let mut tool_result = ToolExecutionResult::success("call-1", "tool ok");
    tool_result
        .metadata
        .insert("path".to_string(), json!("README.md"));
    let cycle = CycleRecord::from_response(
        1,
        &LLMResponse::with_tool_calls(
            "assistant",
            vec![vv_agent::ToolCall::new(
                "call-1",
                "read_file",
                [("path".to_string(), json!("README.md"))]
                    .into_iter()
                    .collect(),
            )],
        ),
        vec![tool_result],
    );
    let result = AgentResult::completed_with_shared_state(
        vec![Message::system("system"), Message::user("user")],
        vec![cycle],
        "done",
        [("todo_list".to_string(), json!([]))].into_iter().collect(),
    );

    let payload = result.to_dict();
    assert_eq!(payload["status"], json!("completed"));
    assert_eq!(
        payload["cycles"][0]["tool_results"][0]["status"],
        json!("success")
    );
    assert_eq!(
        payload["cycles"][0]["tool_results"][0]["status_code"],
        json!("SUCCESS")
    );

    let restored = AgentResult::from_dict(&payload).expect("agent result from dict");
    assert_eq!(restored.status, AgentStatus::Completed);
    assert_eq!(restored.final_answer.as_deref(), Some("done"));
    assert_eq!(restored.messages[1].content, "user");
    assert_eq!(
        restored.cycles[0].tool_results[0].metadata["path"],
        json!("README.md")
    );
}

#[test]
fn agent_result_dict_round_trips_token_usage_cycles() {
    let mut result = AgentResult::completed(vec![Message::user("hi")], vec![], "done");
    result.token_usage.prompt_tokens = 12;
    result.token_usage.completion_tokens = 4;
    result.token_usage.total_tokens = 16;
    result.token_usage.cycles.push(CycleTokenUsage {
        cycle_index: 7,
        usage: TokenUsage {
            prompt_tokens: 12,
            completion_tokens: 4,
            total_tokens: 16,
            raw: json!({"provider": "deepseek"}),
            ..TokenUsage::default()
        },
    });

    let payload = result.to_dict();
    let restored = AgentResult::from_dict(&payload).expect("agent result from dict");

    assert_eq!(restored.token_usage.prompt_tokens, 12);
    assert_eq!(restored.token_usage.cycles.len(), 1);
    assert_eq!(restored.token_usage.cycles[0].cycle_index, 7);
    assert_eq!(
        restored.token_usage.cycles[0].usage.raw["provider"],
        "deepseek"
    );
}

#[test]
fn agent_task_dict_round_trips_python_runtime_recipe_payload_shape() {
    let mut task = AgentTask::new("task-1", "deepseek-v4-pro", "system", "user");
    task.max_cycles = 3;
    task.no_tool_policy = NoToolPolicy::WaitUser;
    task.has_sub_agents = true;
    task.agent_type = Some("computer".to_string());
    task.extra_tool_names.push("read_image".to_string());
    task.metadata.insert("k".to_string(), Value::from("v"));

    let payload = task.to_dict();
    assert_eq!(payload["task_id"], json!("task-1"));
    assert_eq!(payload["no_tool_policy"], json!("wait_user"));
    assert_eq!(payload["has_sub_agents"], json!(true));

    let restored = AgentTask::from_dict(&payload).expect("agent task from dict");
    assert_eq!(restored.task_id, task.task_id);
    assert_eq!(restored.no_tool_policy, NoToolPolicy::WaitUser);
    assert_eq!(restored.agent_type.as_deref(), Some("computer"));
    assert_eq!(restored.extra_tool_names, vec!["read_image"]);
    assert_eq!(restored.metadata["k"], json!("v"));
}

#[test]
fn message_to_openai_message_matches_python_multimodal_and_tool_shapes() {
    let mut assistant = Message::assistant("");
    assistant.reasoning_content = Some("private reasoning".to_string());
    assistant.tool_calls = vec![ToolCall::new(
        "call-1",
        "read_file",
        [("path".to_string(), json!("README.md"))]
            .into_iter()
            .collect(),
    )];

    let assistant_payload = assistant.to_openai_message(true);
    assert_eq!(assistant_payload["role"], json!("assistant"));
    assert_eq!(assistant_payload["content"], Value::Null);
    assert_eq!(
        assistant_payload["reasoning_content"],
        json!("private reasoning")
    );
    assert_eq!(
        assistant_payload["tool_calls"][0]["function"]["arguments"],
        json!(r#"{"path":"README.md"}"#)
    );
    assert!(assistant
        .to_openai_message(false)
        .get("reasoning_content")
        .is_none());

    assistant.tool_calls[0].extra_content = Some(json!({
        "google": {"thought_signature": "sig_123"}
    }));
    let assistant_payload = assistant.to_openai_message(true);
    assert_eq!(
        assistant_payload["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
        json!("sig_123")
    );

    let mut image = Message::user("inspect");
    image.image_url = Some("data:image/png;base64,abc".to_string());
    let image_payload = image.to_openai_message(true);
    assert_eq!(image_payload["role"], json!("user"));
    assert_eq!(
        image_payload["content"][0],
        json!({"type": "text", "text": "inspect"})
    );
    assert_eq!(
        image_payload["content"][1],
        json!({"type": "image_url", "image_url": {"url": "data:image/png;base64,abc"}})
    );
}

#[test]
fn message_to_openai_message_omits_empty_reasoning_like_python() {
    let mut assistant = Message::assistant("answer");
    assistant.reasoning_content = Some(String::new());

    let payload = assistant.to_openai_message(true);

    assert!(
        payload.get("reasoning_content").is_none(),
        "empty Python reasoning_content values should not be serialized into OpenAI payloads"
    );
}

#[test]
fn message_dict_round_trips_python_openai_style_tool_calls() {
    let payload = json!({
        "role": "assistant",
        "content": "",
        "tool_calls": [
            {
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "default_api:list_files",
                    "arguments": "{\"path\":\".\"}"
                },
                "extra_content": {
                    "google": {"thought_signature": "sig_123"}
                }
            }
        ],
    });

    let message = Message::from_dict(&payload).expect("message from python dict");

    assert_eq!(message.tool_calls[0].id, "call_1");
    assert_eq!(message.tool_calls[0].name, "default_api:list_files");
    assert_eq!(message.tool_calls[0].arguments["path"], json!("."));
    assert_eq!(
        message.tool_calls[0]
            .extra_content
            .as_ref()
            .expect("extra content")["google"]["thought_signature"],
        json!("sig_123")
    );

    let serialized = message.to_dict();
    assert_eq!(
        serialized["tool_calls"][0]["function"]["name"],
        json!("default_api:list_files")
    );
    assert_eq!(
        serialized["tool_calls"][0]["function"]["arguments"],
        json!("{\"path\":\".\"}")
    );
    assert_eq!(
        serialized["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
        json!("sig_123")
    );
}

#[test]
fn tool_execution_result_to_tool_message_alias_matches_python() {
    let result = ToolExecutionResult::success("call_1", "ok");

    let message = result.to_tool_message();

    assert_eq!(message.role, vv_agent::MessageRole::Tool);
    assert_eq!(message.tool_call_id.as_deref(), Some("call_1"));
    assert_eq!(message.content, "ok");
}

#[test]
fn sub_task_protocol_helpers_match_python_defaults_and_dict_shape() {
    let request = SubTaskRequest::new("researcher", "collect sources");
    assert_eq!(request.agent_name, "researcher");
    assert_eq!(request.task_description, "collect sources");
    assert_eq!(request.output_requirements, "");
    assert!(!request.include_main_summary);
    assert!(request.exclude_files_pattern.is_none());
    assert!(request.metadata.is_empty());

    let outcome = SubTaskOutcome {
        task_id: "sub-1".to_string(),
        agent_name: "researcher".to_string(),
        status: AgentStatus::Completed,
        session_id: Some("session-1".to_string()),
        final_answer: Some("done".to_string()),
        wait_reason: None,
        error: None,
        cycles: 2,
        todo_list: vec![json!({"title": "collect", "status": "completed"})],
        resolved: BTreeMap::from([("model".to_string(), "deepseek-v4-pro".to_string())]),
    };

    let payload = outcome.to_dict();
    assert_eq!(payload["task_id"], "sub-1");
    assert_eq!(payload["agent_name"], "researcher");
    assert_eq!(payload["status"], "completed");
    assert_eq!(payload["session_id"], "session-1");
    assert_eq!(payload["final_answer"], "done");
    assert_eq!(payload["cycles"], 2);
    assert_eq!(payload["todo_list"][0]["title"], "collect");
    assert_eq!(payload["resolved"]["model"], "deepseek-v4-pro");
}
