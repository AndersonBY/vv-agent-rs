use serde_json::json;
use vv_agent::app_server::client::AppServerClient;
use vv_agent::app_server::protocol::{
    AppClientInfo, AppItemKind, ApprovalDecision, ApprovalResolveParams, JsonRpcMessage, RequestId,
    ServerRequest, ThreadReadParams, ThreadStartParams, TurnStartParams, UserInput,
};
use vv_agent::app_server::test_support::{
    approval_app_server_client, finish_response, scripted_app_server_client,
};

#[tokio::test]
async fn client_initializes_channel_server() {
    let mut client = scripted_app_server_client(vec![finish_response("hello")]);

    let initialized = client.initialize(client_info()).await.expect("initialize");

    assert_eq!(initialized.server_info.name, "vv-agent-rs");
    assert!(initialized.capabilities.thread);
    assert!(initialized.capabilities.event_replay);
}

#[tokio::test]
async fn client_starts_thread() {
    let mut client = initialized_client(vec![finish_response("hello")]).await;

    let response = client
        .start_thread(ThreadStartParams {
            cwd: None,
            title: Some("demo".to_string()),
            model: Some("demo-model".to_string()),
            ephemeral: false,
        })
        .await
        .expect("start thread");

    assert_eq!(response.thread.title.as_deref(), Some("demo"));
    assert_eq!(response.thread.model.as_deref(), Some("demo-model"));
}

#[tokio::test]
async fn client_starts_turn_and_collects_notifications() {
    let mut client = initialized_client(vec![finish_response("hello world")]).await;
    let thread = client
        .start_thread(ThreadStartParams {
            cwd: None,
            title: None,
            model: Some("demo-model".to_string()),
            ephemeral: false,
        })
        .await
        .expect("thread")
        .thread;

    let turn = client
        .start_turn(TurnStartParams {
            thread_id: thread.id.clone(),
            input: vec![UserInput {
                text: "say hello".to_string(),
            }],
            model: Some("demo-model".to_string()),
        })
        .await
        .expect("turn")
        .turn;
    assert_eq!(turn.thread_id, thread.id);

    let mut saw_delta = false;
    let mut saw_completed = false;
    for _ in 0..8 {
        match client.next_message().await.expect("message") {
            JsonRpcMessage::Notification(notification)
                if notification.method == "item/agentMessage/delta" =>
            {
                saw_delta = true;
            }
            JsonRpcMessage::Notification(notification)
                if notification.method == "turn/completed" =>
            {
                saw_completed = true;
                break;
            }
            _ => {}
        }
    }

    assert!(saw_delta);
    assert!(saw_completed);
}

#[tokio::test]
async fn client_resolves_approval_request() {
    let mut client = approval_app_server_client();
    client.initialize(client_info()).await.expect("initialize");
    let thread = client
        .start_thread(ThreadStartParams {
            cwd: None,
            title: None,
            model: Some("approval-model".to_string()),
            ephemeral: false,
        })
        .await
        .expect("thread")
        .thread;
    let turn = client
        .start_turn(TurnStartParams {
            thread_id: thread.id.clone(),
            input: vec![UserInput {
                text: "run approval tool".to_string(),
            }],
            model: Some("approval-model".to_string()),
        })
        .await
        .expect("turn")
        .turn;

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

    client
        .resolve_approval(ApprovalResolveParams {
            thread_id: thread.id,
            turn_id: turn.id,
            request_id,
            decision: ApprovalDecision::Allow,
        })
        .await
        .expect("resolve approval");

    let mut saw_resolved = false;
    let mut saw_completed = false;
    for _ in 0..8 {
        match client.next_message().await.expect("message") {
            JsonRpcMessage::Notification(notification)
                if notification.method == "approval/resolved" =>
            {
                saw_resolved = true;
            }
            JsonRpcMessage::Notification(notification)
                if notification.method == "turn/completed" =>
            {
                saw_completed = true;
                break;
            }
            _ => {}
        }
    }

    assert!(saw_resolved);
    assert!(saw_completed);
}

#[tokio::test]
async fn client_reads_thread_history() {
    let mut client = initialized_client(vec![finish_response("hello history")]).await;
    let thread = client
        .start_thread(ThreadStartParams {
            cwd: None,
            title: None,
            model: Some("demo-model".to_string()),
            ephemeral: false,
        })
        .await
        .expect("thread")
        .thread;
    client
        .start_turn(TurnStartParams {
            thread_id: thread.id.clone(),
            input: vec![UserInput {
                text: "write history".to_string(),
            }],
            model: Some("demo-model".to_string()),
        })
        .await
        .expect("turn");

    for _ in 0..8 {
        if matches!(
            client.next_message().await.expect("message"),
            JsonRpcMessage::Notification(notification) if notification.method == "turn/completed"
        ) {
            break;
        }
    }

    let history = client
        .read_thread(ThreadReadParams {
            thread_id: thread.id,
            after_item_id: None,
        })
        .await
        .expect("history");

    assert!(history
        .items
        .iter()
        .any(|item| item.kind == AppItemKind::AgentMessage));
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
