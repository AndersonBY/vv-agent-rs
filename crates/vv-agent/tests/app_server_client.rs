use serde_json::json;
use std::time::Duration;
use vv_agent::app_server::processor::MessageProcessor;
use vv_agent::app_server::protocol::{
    AppClientInfo, AppItemKind, AppServerError, AppServerErrorCode, ApprovalDecision,
    ApprovalRequestParams, ApprovalResolveParams, ClientRequest, JsonRpcMessage, ModelListParams,
    RequestId, ServerRequest, ThreadArchiveParams, ThreadListParams, ThreadReadParams,
    ThreadResumeParams, ThreadStartParams, ThreadStatus, ThreadUnsubscribeParams,
    TurnFollowUpParams, TurnInterruptParams, TurnStartParams, TurnSteerParams,
};
use vv_agent::app_server::test_support::{
    approval_app_server_client, finish_response, scripted_app_server_client,
};
use vv_agent::app_server::transport::ConnectionId;
use vv_agent::app_server::{AppServerClient, AppServerClientError};

#[tokio::test]
async fn client_initializes_channel_server() {
    let mut client = scripted_app_server_client(vec![finish_response("hello")]);

    let initialized = client.initialize(client_info()).await.expect("initialize");

    assert_eq!(initialized.user_agent, "vv-agent-app-server");
    assert_eq!(initialized.protocol_version, "v1");
    assert!(initialized.capabilities.thread_lifecycle);
}

#[tokio::test]
async fn client_covers_thread_model_schema_and_close_lifecycle() {
    let mut client = initialized_client(vec![finish_response("hello")]).await;

    let response = client
        .start_thread(ThreadStartParams {
            agent_key: "default".to_string(),
            cwd: None,
            metadata: Default::default(),
        })
        .await
        .expect("start thread");

    assert_eq!(response.thread_id, "thread_1");
    assert_eq!(response.agent_key, "default");
    let thread_id = response.thread_id;

    let resumed = client
        .resume_thread(ThreadResumeParams {
            thread_id: thread_id.clone(),
            subscribe: true,
        })
        .await
        .expect("resume thread");
    assert_eq!(resumed.thread.thread_id, thread_id);

    let read = client
        .read_thread(ThreadReadParams {
            thread_id: thread_id.clone(),
            after_item_id: None,
        })
        .await
        .expect("read thread");
    assert_eq!(read.thread.thread_id, thread_id);

    let listed = client
        .list_threads(ThreadListParams::default())
        .await
        .expect("list threads");
    assert!(listed
        .threads
        .iter()
        .any(|thread| thread.thread_id == thread_id));

    client
        .list_models(ModelListParams::default())
        .await
        .expect("list models");
    let schema = client.export_schema().await.expect("export schema");
    assert!(schema.json_schema.contains_key("ClientRequest"));
    assert!(schema.typescript.contains_key("ClientRequest.ts"));

    let unsubscribed = client
        .unsubscribe_thread(ThreadUnsubscribeParams {
            thread_id: thread_id.clone(),
        })
        .await
        .expect("unsubscribe thread");
    assert!(!unsubscribed.subscribed);
    assert!(unsubscribed.closed);

    let resumed = client
        .resume_thread(ThreadResumeParams {
            thread_id: thread_id.clone(),
            subscribe: true,
        })
        .await
        .expect("resume closed thread");
    assert_eq!(resumed.thread.status, ThreadStatus::Idle);

    let archived = client
        .archive_thread(ThreadArchiveParams { thread_id })
        .await
        .expect("archive thread");
    assert!(archived.archived);
    assert!(client.close().await);
    assert!(!client.close().await);

    let closed_error = client
        .list_models(ModelListParams::default())
        .await
        .expect_err("closed client must reject requests");
    assert_eq!(closed_error.message(), "App Server client is closed");
    assert!(client.next_message().await.is_none());
    assert_eq!(
        client
            .try_next_message()
            .await
            .expect_err("closed client must reject reads")
            .message(),
        "App Server client is closed"
    );
}

#[tokio::test]
async fn client_starts_turn_and_collects_notifications() {
    let mut client = initialized_client(vec![finish_response("hello world")]).await;
    let thread = client
        .start_thread(ThreadStartParams {
            agent_key: "default".to_string(),
            cwd: None,
            metadata: Default::default(),
        })
        .await
        .expect("thread")
        .thread_id;

    let turn = client
        .start_turn(TurnStartParams {
            thread_id: thread.clone(),
            input: vec![json!({"type": "text", "text": "say hello"})],
            metadata: Default::default(),
        })
        .await
        .expect("turn");
    assert_eq!(turn.thread_id, thread);
    client
        .list_models(ModelListParams::default())
        .await
        .expect("model response while notifications are queued");

    let (saw_delta, saw_agent_message, saw_completed) =
        tokio::time::timeout(Duration::from_secs(3), async {
            let mut saw_delta = false;
            let mut saw_agent_message = false;
            loop {
                match client.next_message().await.expect("message") {
                    JsonRpcMessage::Notification(notification)
                        if notification.method == "item/agentMessage/delta" =>
                    {
                        saw_delta = true;
                    }
                    JsonRpcMessage::Notification(notification)
                        if notification.method == "item/completed"
                            && notification
                                .params
                                .as_ref()
                                .and_then(|params| params.get("type"))
                                .and_then(serde_json::Value::as_str)
                                == Some("agentMessage") =>
                    {
                        saw_agent_message = true;
                    }
                    JsonRpcMessage::Notification(notification)
                        if notification.method == "turn/completed" =>
                    {
                        break (saw_delta, saw_agent_message, true);
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("turn completion timeout");

    assert!(!saw_delta);
    assert!(saw_agent_message);
    assert!(saw_completed);
    assert!(client.close().await);
}

#[tokio::test]
async fn client_preserves_allow_session_approval_decision() {
    let (mut client, params) = pending_approval_client().await;

    client
        .resolve_approval(params)
        .await
        .expect("resolve approval");
    assert_allow_session_completion(&mut client).await;
    assert!(client.close().await);
}

#[tokio::test]
async fn client_resolves_approval_through_stable_request_method() {
    let (mut client, params) = pending_approval_client().await;

    client
        .resolve_approval_request(params)
        .await
        .expect("resolve approval request");
    assert_allow_session_completion(&mut client).await;
    assert!(client.close().await);
}

#[tokio::test]
async fn client_reads_thread_history() {
    let mut client = initialized_client(vec![finish_response("hello history")]).await;
    let thread = client
        .start_thread(ThreadStartParams {
            agent_key: "default".to_string(),
            cwd: None,
            metadata: Default::default(),
        })
        .await
        .expect("thread")
        .thread_id;
    client
        .start_turn(TurnStartParams {
            thread_id: thread.clone(),
            input: vec![json!({"type": "text", "text": "write history"})],
            metadata: Default::default(),
        })
        .await
        .expect("turn");

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if matches!(
                client.next_message().await.expect("message"),
                JsonRpcMessage::Notification(notification) if notification.method == "turn/completed"
            ) {
                break;
            }
        }
    })
    .await
    .expect("history turn completion timeout");

    let history = client
        .read_thread(ThreadReadParams {
            thread_id: thread,
            after_item_id: None,
        })
        .await
        .expect("history");

    assert!(history
        .items
        .iter()
        .any(|item| item.kind == AppItemKind::AgentMessage));
    assert!(client.close().await);
}

#[tokio::test]
async fn client_exposes_turn_control_methods_and_server_errors() {
    let mut client = initialized_client(vec![finish_response("unused")]).await;
    let thread_id = client
        .start_thread(ThreadStartParams::default())
        .await
        .expect("thread")
        .thread_id;

    let error = client
        .steer_turn(TurnSteerParams {
            thread_id: thread_id.clone(),
            expected_turn_id: "turn_1".to_string(),
            input: Vec::new(),
        })
        .await
        .expect_err("steer without active turn must fail");
    assert_server_error(&error, AppServerErrorCode::ActiveTurnNotFound);

    let error = client
        .follow_up_turn(TurnFollowUpParams {
            thread_id: thread_id.clone(),
            expected_turn_id: "turn_1".to_string(),
            input: Vec::new(),
        })
        .await
        .expect_err("follow-up without active turn must fail");
    assert_server_error(&error, AppServerErrorCode::ActiveTurnNotFound);

    let error = client
        .interrupt_turn(TurnInterruptParams {
            thread_id: thread_id.clone(),
            expected_turn_id: "turn_1".to_string(),
            reason: String::new(),
        })
        .await
        .expect_err("interrupt without active turn must fail");
    assert_server_error(&error, AppServerErrorCode::ActiveTurnNotFound);

    assert!(client.close().await);
}

#[tokio::test]
async fn client_error_preserves_server_code_and_data() {
    let (processor, outgoing_rx) = MessageProcessor::new_for_tests(16);
    let outgoing = processor.outgoing().clone();
    let connection_id = ConnectionId::new(41);
    let mut client = AppServerClient::new_for_processor(processor, outgoing_rx, connection_id);
    client.initialize(client_info()).await.expect("initialize");

    let expected_data = json!({"method": "missing/method"});
    outgoing
        .send_error(
            connection_id,
            RequestId::Integer(2),
            AppServerError::new(AppServerErrorCode::MethodNotFound, "Method not found")
                .with_data(expected_data.clone()),
        )
        .await
        .expect("queue synthetic server error");

    let error = client
        .list_models(ModelListParams::default())
        .await
        .expect_err("synthetic server error must win queue order");
    assert_eq!(
        error.code(),
        Some(AppServerErrorCode::MethodNotFound.code())
    );
    assert_eq!(error.data(), Some(&expected_data));
    assert_eq!(error.message(), "Method not found");
    assert!(client.close().await);
}

#[tokio::test]
async fn client_close_disconnects_and_clears_pending_server_requests() {
    let (processor, outgoing_rx) = MessageProcessor::new_for_tests(16);
    let outgoing = processor.outgoing().clone();
    let connection_id = ConnectionId::new(42);
    let mut client = AppServerClient::new_for_processor(processor, outgoing_rx, connection_id);
    client.initialize(client_info()).await.expect("initialize");

    let request_id = RequestId::String("approval_close".to_string());
    let callback = outgoing
        .send_server_request_with_id(
            connection_id,
            request_id,
            ServerRequest::ApprovalRequest(ApprovalRequestParams {
                request_id: "approval_close".to_string(),
                thread_id: "thread_1".to_string(),
                turn_id: "turn_1".to_string(),
                tool_call_id: "call_1".to_string(),
                tool_name: "dangerous".to_string(),
                preview: "Approval required".to_string(),
                arguments: json!({}),
            }),
        )
        .await
        .expect("server request");
    assert_eq!(outgoing.pending_server_request_count().await, 1);

    assert!(client.close().await);
    assert!(!client.close().await);
    assert_eq!(outgoing.pending_server_request_count().await, 0);
    let callback_error = callback
        .await
        .expect("callback sender")
        .expect_err("disconnect must reject pending request");
    assert_eq!(callback_error.message, "client_disconnected");
    assert!(client.next_message().await.is_none());
    assert_eq!(
        client
            .try_next_message()
            .await
            .expect_err("closed read must fail")
            .message(),
        "App Server client is closed"
    );
}

#[test]
fn client_facade_covers_stable_method_inventory() {
    assert_eq!(
        ClientRequest::stable_method_names(),
        vec![
            "initialize",
            "thread/start",
            "thread/resume",
            "thread/read",
            "thread/list",
            "thread/archive",
            "thread/unsubscribe",
            "turn/start",
            "turn/interrupt",
            "turn/steer",
            "turn/followUp",
            "approval/resolve",
            "model/list",
            "schema/export",
            "initialized",
        ]
    );

    let _ = AppServerClient::initialize;
    let _ = AppServerClient::start_thread;
    let _ = AppServerClient::resume_thread;
    let _ = AppServerClient::read_thread;
    let _ = AppServerClient::list_threads;
    let _ = AppServerClient::archive_thread;
    let _ = AppServerClient::unsubscribe_thread;
    let _ = AppServerClient::start_turn;
    let _ = AppServerClient::interrupt_turn;
    let _ = AppServerClient::steer_turn;
    let _ = AppServerClient::follow_up_turn;
    let _ = AppServerClient::resolve_approval_request;
    let _ = AppServerClient::list_models;
    let _ = AppServerClient::export_schema;
    let _ = AppServerClient::send_response;
    let _ = AppServerClient::next_message;
    let _ = AppServerClient::close;
}

async fn pending_approval_client() -> (AppServerClient, ApprovalResolveParams) {
    let mut client = approval_app_server_client();
    client.initialize(client_info()).await.expect("initialize");
    let thread_id = client
        .start_thread(ThreadStartParams::default())
        .await
        .expect("thread")
        .thread_id;
    let turn_id = client
        .start_turn(TurnStartParams {
            thread_id: thread_id.clone(),
            input: vec![json!({"type": "text", "text": "run approval tool"})],
            metadata: Default::default(),
        })
        .await
        .expect("turn")
        .turn_id;

    let approval_request = loop {
        let message = client.next_message().await.expect("message");
        if let JsonRpcMessage::Request(request) = message {
            let server_request: ServerRequest = serde_json::from_value(json!({
                "method": request.method,
                "params": request.params,
            }))
            .expect("server request");
            if matches!(server_request, ServerRequest::ApprovalRequest(_)) {
                break request;
            }
        }
    };
    let RequestId::String(request_id) = approval_request.id else {
        panic!("approval id should be string");
    };
    let params = ApprovalResolveParams {
        thread_id,
        turn_id,
        request_id,
        decision: ApprovalDecision::AllowSession,
        reason: "approved by owner".to_string(),
        metadata: [("ticket".to_string(), json!(7))].into_iter().collect(),
    };
    (client, params)
}

async fn assert_allow_session_completion(client: &mut AppServerClient) {
    let (resolved_payloads, saw_completed) = tokio::time::timeout(Duration::from_secs(3), async {
        let mut resolved_payloads = Vec::new();
        loop {
            match client.next_message().await.expect("message") {
                JsonRpcMessage::Notification(notification)
                    if notification.method == "approval/resolved" =>
                {
                    resolved_payloads
                        .push(notification.params.expect("approval resolution params"));
                }
                JsonRpcMessage::Notification(notification)
                    if notification.method == "turn/completed" =>
                {
                    break (resolved_payloads, true);
                }
                _ => {}
            }
        }
    })
    .await
    .expect("approval turn completion timeout");

    assert_eq!(resolved_payloads.len(), 1);
    assert_eq!(resolved_payloads[0]["decision"], "allow_session");
    assert_eq!(resolved_payloads[0]["reason"], "approved by owner");
    assert_eq!(resolved_payloads[0]["metadata"], json!({"ticket": 7}));
    assert!(saw_completed);
}

fn assert_server_error(error: &AppServerClientError, expected: AppServerErrorCode) {
    assert_eq!(error.code(), Some(expected.code()));
    assert!(error.data().is_none());
}

async fn initialized_client(responses: Vec<vv_agent::LLMResponse>) -> AppServerClient {
    let mut client = scripted_app_server_client(responses);
    client.initialize(client_info()).await.expect("initialize");
    client
}

fn client_info() -> AppClientInfo {
    AppClientInfo {
        name: "test_client".to_string(),
        title: Some("Test Client".to_string()),
        version: Some("1.0.0".to_string()),
    }
}
