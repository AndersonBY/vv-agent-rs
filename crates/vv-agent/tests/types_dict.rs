use std::collections::BTreeMap;

use serde_json::{json, Value};
use vv_agent::types::AgentTask;
use vv_agent::{
    AgentResult, AgentStatus, CycleRecord, LLMResponse, Message, ModelCallOperation,
    ModelCallRecord, ModelCallStatus, NoToolPolicy, SubTaskOutcome, SubTaskRequest, TokenUsage,
    ToolCall, ToolDirective, ToolExecutionResult, ToolResultStatus,
};

fn sparse_agent_task_payload() -> Value {
    json!({
        "task_id": "task-1",
        "model": "model-1",
        "system_prompt": "system",
        "user_prompt": "user"
    })
}

fn assert_agent_task_defaults(task: &AgentTask) {
    assert_eq!(task, &AgentTask::new("task-1", "model-1", "system", "user"));
    assert_eq!(task.memory_compact_threshold, 250_000);
}

#[test]
fn tool_execution_result_dict_matches_status_shape() {
    let success = ToolExecutionResult::success("call-1", "ok");
    let success_dict = success.to_dict();
    assert!(success_dict.get("status").is_none());
    assert_eq!(success_dict["status_code"], json!("SUCCESS"));
    assert_eq!(success_dict["directive"], json!("continue"));

    let mut wait = ToolExecutionResult::success("call-2", "wait");
    wait.status = ToolResultStatus::WaitResponse;
    wait.directive = ToolDirective::WaitUser;
    let wait_dict = wait.to_dict();
    assert!(wait_dict.get("status").is_none());
    assert_eq!(wait_dict["status_code"], json!("WAIT_RESPONSE"));
    assert_eq!(wait_dict["directive"], json!("wait_user"));

    let error = ToolExecutionResult::error("call-3", "bad");
    let error_dict = error.to_dict();
    assert!(error_dict.get("status").is_none());
    assert_eq!(error_dict["status_code"], json!("ERROR"));
}

#[test]
fn tool_execution_result_rejects_non_current_fields() {
    let non_current_status = json!({
        "tool_call_id": "simple-call",
        "status": "done",
        "content": "simple ok",
        "directive": "continue"
    });
    let error = ToolExecutionResult::from_dict(&non_current_status)
        .expect_err("non-current status rejected");
    assert!(error.contains("missing=[\"status_code\"]"));
    assert!(error.contains("unknown=[\"status\"]"));

    let unknown_field = json!({
        "tool_call_id": "simple-call",
        "content": "simple ok",
        "status_code": "SUCCESS",
        "directive": "continue",
        "compatibility_hint": true
    });
    assert!(ToolExecutionResult::from_dict(&unknown_field).is_err());
}

#[test]
fn agent_result_dict_round_trips_agent_result_payload_shape() {
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
    assert!(payload["cycles"][0]["tool_results"][0]
        .get("status")
        .is_none());
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
fn agent_result_dict_round_trips_model_call_usage() {
    let mut result = AgentResult::completed(vec![Message::user("hi")], vec![], "done");
    result
        .token_usage
        .add_model_call(ModelCallRecord {
            call_id: "op_model_cycle_7_main:attempt:1".to_string(),
            operation_id: "op_model_cycle_7_main".to_string(),
            attempt: 1,
            operation: ModelCallOperation::AgentCycle,
            cycle_index: 7,
            backend: "deepseek".to_string(),
            model: "deepseek-v4-pro".to_string(),
            status: ModelCallStatus::Completed,
            usage: TokenUsage {
                input_tokens: Some(12),
                output_tokens: Some(4),
                total_tokens: Some(16),
                provider_usage: json!({"provider": "deepseek"})
                    .as_object()
                    .expect("provider usage")
                    .clone(),
                ..TokenUsage::default()
            },
            error_code: None,
        })
        .expect("unique model call");

    let payload = result.to_dict();
    let restored = AgentResult::from_dict(&payload).expect("agent result from dict");

    assert_eq!(restored.token_usage.input_tokens, Some(12));
    assert_eq!(restored.token_usage.model_calls.len(), 1);
    assert_eq!(restored.token_usage.model_calls[0].cycle_index, 7);
    assert_eq!(
        restored.token_usage.model_calls[0].usage.provider_usage["provider"],
        "deepseek"
    );
}

#[test]
fn agent_task_dict_round_trips_agent_runtime_recipe_payload_shape() {
    let mut task = AgentTask::new("task-1", "deepseek-v4-pro", "system", "user");
    task.max_cycles = 3;
    task.no_tool_policy = NoToolPolicy::WaitUser;
    task.agent_type = Some("computer".to_string());
    task.extra_tool_names.push("read_image".to_string());
    task.metadata.insert("k".to_string(), Value::from("v"));

    let payload = task.to_dict();
    assert_eq!(payload["task_id"], json!("task-1"));
    assert_eq!(payload["no_tool_policy"], json!("wait_user"));
    assert!(payload.get("has_sub_agents").is_none());

    let restored = AgentTask::from_dict(&payload).expect("agent task from dict");
    assert_eq!(restored.task_id, task.task_id);
    assert_eq!(restored.no_tool_policy, NoToolPolicy::WaitUser);
    assert_eq!(restored.agent_type.as_deref(), Some("computer"));
    assert_eq!(restored.extra_tool_names, vec!["read_image"]);
    assert_eq!(restored.metadata["k"], json!("v"));
}

#[test]
fn agent_task_sparse_wire_uses_current_defaults_for_dict_and_serde() {
    let payload = sparse_agent_task_payload();

    let from_dict = AgentTask::from_dict(&payload).expect("sparse AgentTask dict");
    let from_serde: AgentTask = serde_json::from_value(payload).expect("sparse AgentTask serde");

    assert_agent_task_defaults(&from_dict);
    assert_agent_task_defaults(&from_serde);
}

#[test]
fn agent_task_preserves_explicit_memory_threshold() {
    let mut payload = sparse_agent_task_payload();
    payload["memory_compact_threshold"] = json!(128_000);

    let from_dict = AgentTask::from_dict(&payload).expect("historical AgentTask dict");
    let from_serde: AgentTask =
        serde_json::from_value(payload).expect("historical AgentTask serde");

    assert_eq!(from_dict.memory_compact_threshold, 128_000);
    assert_eq!(from_serde.memory_compact_threshold, 128_000);
}

#[test]
fn agent_task_wire_requires_all_core_string_fields() {
    for field_name in ["task_id", "model", "system_prompt", "user_prompt"] {
        let mut missing = sparse_agent_task_payload();
        missing
            .as_object_mut()
            .expect("AgentTask object")
            .remove(field_name);
        assert!(
            AgentTask::from_dict(&missing).is_err(),
            "from_dict accepted missing {field_name}"
        );
        assert!(
            serde_json::from_value::<AgentTask>(missing).is_err(),
            "serde accepted missing {field_name}"
        );

        let mut wrong_type = sparse_agent_task_payload();
        wrong_type[field_name] = json!(123);
        assert!(
            AgentTask::from_dict(&wrong_type).is_err(),
            "from_dict accepted non-string {field_name}"
        );
        assert!(
            serde_json::from_value::<AgentTask>(wrong_type).is_err(),
            "serde accepted non-string {field_name}"
        );
    }
}

#[test]
fn agent_task_wire_rejects_wrong_types_ranges_and_container_items() {
    let above_u64 = serde_json::from_str::<Value>("18446744073709551616")
        .expect("JSON integer above u64 range");
    let invalid_values = vec![
        ("max_cycles", json!(true)),
        ("max_cycles", json!(-1)),
        ("max_cycles", json!(u64::from(u32::MAX) + 1)),
        ("max_cycles", json!(1.5)),
        ("memory_compact_threshold", json!(true)),
        ("memory_compact_threshold", json!(-1)),
        ("memory_compact_threshold", above_u64),
        ("memory_compact_threshold", json!(1.5)),
        ("memory_threshold_percentage", json!(true)),
        ("memory_threshold_percentage", json!(-1)),
        ("memory_threshold_percentage", json!(256)),
        ("memory_threshold_percentage", json!(1.5)),
        ("no_tool_policy", json!(1)),
        ("no_tool_policy", json!("invalid")),
        ("allow_interruption", json!(1)),
        ("use_workspace", json!(1)),
        ("has_sub_agents", json!(true)),
        ("sub_agents", json!([])),
        ("sub_agents", json!({"research": "not-an-object"})),
        ("agent_type", json!(1)),
        ("native_multimodal", json!(1)),
        ("extra_tool_names", json!("read_file")),
        ("extra_tool_names", json!(["read_file", 1])),
        ("exclude_tools", json!("write_file")),
        ("exclude_tools", json!(["write_file", 1])),
        ("model_settings", json!([])),
        ("initial_messages", json!({})),
        ("initial_messages", json!([1])),
        ("initial_shared_state", json!([])),
        ("metadata", json!([])),
    ];

    for (field_name, invalid_value) in invalid_values {
        let mut payload = sparse_agent_task_payload();
        payload[field_name] = invalid_value;
        assert!(
            AgentTask::from_dict(&payload).is_err(),
            "from_dict accepted invalid {field_name}: {}",
            payload[field_name]
        );
        assert!(
            serde_json::from_value::<AgentTask>(payload.clone()).is_err(),
            "serde accepted invalid {field_name}: {}",
            payload[field_name]
        );
    }
}

#[test]
fn agent_task_wire_accepts_unsigned_boundaries() {
    let mut payload = sparse_agent_task_payload();
    payload["max_cycles"] = json!(u32::MAX);
    payload["memory_compact_threshold"] = json!(u64::MAX);
    payload["memory_threshold_percentage"] = json!(u8::MAX);

    let from_dict = AgentTask::from_dict(&payload).expect("AgentTask dict boundaries");
    let from_serde: AgentTask =
        serde_json::from_value(payload).expect("AgentTask serde boundaries");

    for task in [&from_dict, &from_serde] {
        assert_eq!(task.max_cycles, u32::MAX);
        assert_eq!(task.memory_compact_threshold, u64::MAX);
        assert_eq!(task.memory_threshold_percentage, u8::MAX);
    }
}

#[test]
fn agent_task_round_trips_full_serde_and_compact_dict_wire() {
    let mut task = AgentTask::new("task-full", "model", "system", "user");
    task.model_settings = Some(vv_agent::ModelSettings::builder().max_tokens(512).build());
    task.initial_messages = vec![Message::user("persisted")];
    task.initial_shared_state
        .insert("scope".to_string(), json!("child"));
    task.metadata
        .insert("trace_id".to_string(), json!("trace-1"));

    let serde_payload = serde_json::to_value(&task).expect("serialize AgentTask");
    let serde_restored: AgentTask =
        serde_json::from_value(serde_payload).expect("deserialize AgentTask");
    let dict_restored =
        AgentTask::from_dict(&task.to_dict()).expect("deserialize compact AgentTask dict");

    assert_eq!(serde_restored, task);
    assert_eq!(dict_restored, task);
}

#[test]
fn message_to_openai_message_matches_multimodal_and_tool_shapes() {
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
fn message_to_openai_message_omits_empty_reasoning() {
    let mut assistant = Message::assistant("answer");
    assistant.reasoning_content = Some(String::new());

    let payload = assistant.to_openai_message(true);

    assert!(
        payload.get("reasoning_content").is_none(),
        "empty reasoning_content values should not be serialized into OpenAI payloads"
    );
}

#[test]
fn message_to_openai_message_omits_empty_optional_fields() {
    let mut user = Message::user("inspect");
    user.name = Some(String::new());
    user.tool_call_id = Some(String::new());
    user.image_url = Some(String::new());

    let payload = user.to_openai_message(true);

    assert!(payload.get("name").is_none());
    assert!(payload.get("tool_call_id").is_none());
    assert_eq!(payload["content"], json!("inspect"));
}

#[test]
fn message_dict_round_trips_agent_openai_style_tool_calls() {
    let payload = json!({
        "role": "assistant",
        "content": "",
        "tool_calls": [
            {
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "default_api:find_files",
                    "arguments": "{\"path\":\".\"}"
                },
                "extra_content": {
                    "google": {"thought_signature": "sig_123"}
                }
            }
        ],
    });

    let message = Message::from_dict(&payload).expect("message from dict");

    assert_eq!(message.tool_calls[0].id, "call_1");
    assert_eq!(message.tool_calls[0].name, "default_api:find_files");
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
        json!("default_api:find_files")
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
fn tool_execution_result_to_tool_message_preserves_alias() {
    let result = ToolExecutionResult::success("call_1", "ok");

    let message = result.to_tool_message();

    assert_eq!(message.role, vv_agent::MessageRole::Tool);
    assert_eq!(message.tool_call_id.as_deref(), Some("call_1"));
    assert_eq!(message.content, "ok");
}

#[test]
fn sub_task_protocol_helpers_match_agent_defaults_and_dict_shape() {
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
        error_code: None,
        completion_reason: None,
        completion_tool_name: None,
        partial_output: None,
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
