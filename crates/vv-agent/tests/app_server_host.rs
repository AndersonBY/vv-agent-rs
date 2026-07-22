use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::mpsc;
use vv_agent::app_server::host::{
    AgentResolutionRequest, AppServerHost, AppServerHostError, DefaultAppServerHost,
    RunConfigResolutionRequest,
};
use vv_agent::app_server::outgoing::OutgoingEnvelope;
use vv_agent::app_server::processor::MessageProcessor;
use vv_agent::app_server::protocol::{
    AppModelInfo, JsonRpcMessage, JsonRpcRequest, ModelListParams, ModelListResponse, RequestId,
    TurnStartResponse,
};
use vv_agent::app_server::thread_store::SqliteThreadStore;
use vv_agent::app_server::transport::ConnectionId;
use vv_agent::{Agent, LLMResponse, ModelRef, RunConfig, Runner, ScriptedModelProvider, ToolCall};

#[tokio::test]
async fn model_list_uses_the_injected_host() {
    let host = DefaultAppServerHost::new().with_models(vec![AppModelInfo {
        id: "provider-model".to_string(),
        provider: Some("test-provider".to_string()),
        display_name: Some("Provider Model".to_string()),
        context_length: Some(128_000),
        supports_tools: true,
        metadata: Default::default(),
    }]);
    let (mut processor, mut outgoing) = MessageProcessor::new_with_host(16, Arc::new(host));
    let connection_id = ConnectionId::new(1);
    initialize(&mut processor, &mut outgoing, connection_id).await;

    processor
        .process_message(
            connection_id,
            request(2, "model/list", json!({"provider": "test-provider"})),
        )
        .await;

    let result = expect_response(&mut outgoing, RequestId::Integer(2)).await;
    assert_eq!(result["models"][0]["id"], "provider-model");
    assert_eq!(result["models"][0]["provider"], "test-provider");
    assert_eq!(result["models"][0]["displayName"], "Provider Model");
}

#[tokio::test]
async fn model_list_host_failures_are_canonical_internal_errors() {
    let (mut processor, mut outgoing) = MessageProcessor::new_with_host(
        16,
        Arc::new(FailingHost {
            stage: FailureStage::Models,
        }),
    );
    let connection_id = ConnectionId::new(1);
    initialize(&mut processor, &mut outgoing, connection_id).await;

    processor
        .process_message(connection_id, request(2, "model/list", json!({})))
        .await;

    let error = expect_error(&mut outgoing, RequestId::Integer(2)).await;
    assert_eq!(error["code"], -32603);
    assert_eq!(error["message"], "model list failed");
}

#[tokio::test]
async fn host_resolution_failures_are_canonical_internal_errors_without_creating_turns() {
    for (stage, expected_message) in [
        (FailureStage::Agent, "agent resolution failed"),
        (FailureStage::RunConfig, "run config resolution failed"),
    ] {
        let store = SqliteThreadStore::in_memory().expect("store");
        let runner = scripted_runner(Vec::new());
        let (mut processor, mut outgoing) =
            MessageProcessor::with_host(32, runner, Arc::new(FailingHost { stage }), store.clone());
        let connection_id = ConnectionId::new(1);
        initialize(&mut processor, &mut outgoing, connection_id).await;
        start_thread(&mut processor, &mut outgoing, connection_id).await;

        processor
            .process_message(
                connection_id,
                request(
                    3,
                    "turn/start",
                    json!({
                        "threadId": "thread_1",
                        "input": [{"type": "text", "text": "hello"}]
                    }),
                ),
            )
            .await;

        let error = expect_error(&mut outgoing, RequestId::Integer(3)).await;
        assert_eq!(error["code"], -32603);
        assert_eq!(error["message"], expected_message);
        assert!(store.list_turns("thread_1").expect("turns").is_empty());
    }
}

#[tokio::test]
async fn host_resolves_agent_and_run_config_for_every_turn() {
    let state = Arc::new(ResolutionState::default());
    let host = DynamicHost {
        state: state.clone(),
    };
    let (mut processor, mut outgoing) = MessageProcessor::with_host(
        128,
        scripted_runner(Vec::new()),
        Arc::new(host),
        SqliteThreadStore::in_memory().expect("store"),
    );
    let connection_id = ConnectionId::new(1);
    initialize(&mut processor, &mut outgoing, connection_id).await;
    processor
        .process_message(
            connection_id,
            request(
                2,
                "thread/start",
                json!({
                    "agentKey": "tenant-agent",
                    "cwd": "/tmp/tenant-workspace",
                    "metadata": {
                        "tenant": "acme",
                        "role": "thread",
                        "thread_id": "spoofed-thread",
                        "turn_id": "spoofed-thread-turn",
                        "session_id": "spoofed-session"
                    }
                }),
            ),
        )
        .await;
    let _ = expect_response(&mut outgoing, RequestId::Integer(2)).await;
    let _ = expect_notification(&mut outgoing, "thread/started").await;

    for (id, expected_output) in [(3, "resolved-turn-1"), (4, "resolved-turn-2")] {
        processor
            .process_message(
                connection_id,
                request(
                    id,
                    "turn/start",
                    json!({
                        "threadId": "thread_1",
                        "input": [{"type": "text", "text": format!("prompt-{id}")}],
                        "metadata": {
                            "requestId": id,
                            "role": "turn",
                            "thread_id": "spoofed-turn-thread",
                            "turn_id": "spoofed-turn",
                            "session_id": "spoofed-turn-session"
                        }
                    }),
                ),
            )
            .await;
        let response = expect_response(&mut outgoing, RequestId::Integer(id)).await;
        let turn: TurnStartResponse = serde_json::from_value(response).expect("turn response");
        let completed = expect_notification(&mut outgoing, "turn/completed").await;
        assert_eq!(completed["finalOutput"], expected_output);
        assert_eq!(turn.turn_id, format!("turn_{}", id - 2));
    }

    let agent_requests = state.agent_requests.lock().expect("agent requests");
    let config_requests = state.config_requests.lock().expect("config requests");
    assert_eq!(agent_requests.len(), 2);
    assert_eq!(config_requests.len(), 2);
    for (request, request_id) in agent_requests.iter().zip([3, 4]) {
        assert_resolution_request(
            &request.thread_id,
            &request.agent_key,
            request.cwd.as_ref(),
            &request.metadata,
            request_id,
        );
    }
    for (request, request_id) in config_requests.iter().zip([3, 4]) {
        assert_resolution_request(
            &request.thread_id,
            &request.agent_key,
            request.cwd.as_ref(),
            &request.metadata,
            request_id,
        );
    }
    let runtime_metadata = state.runtime_metadata.lock().expect("runtime metadata");
    assert_eq!(runtime_metadata.len(), 2);
    for (index, metadata) in runtime_metadata.iter().enumerate() {
        assert_eq!(metadata["tenant"], "acme");
        assert_eq!(metadata["role"], "turn");
        assert_eq!(metadata["requestId"], json!(index + 3));
        assert_eq!(metadata["thread_id"], "thread_1");
        assert_eq!(metadata["turn_id"], format!("turn_{}", index + 1));
        assert_eq!(metadata["session_id"], "thread_1");
    }
}

#[derive(Clone, Copy)]
enum FailureStage {
    Agent,
    RunConfig,
    Models,
}

struct FailingHost {
    stage: FailureStage,
}

impl AppServerHost for FailingHost {
    fn resolve_agent(&self, request: &AgentResolutionRequest) -> Result<Agent, AppServerHostError> {
        if matches!(self.stage, FailureStage::Agent) {
            return Err(AppServerHostError::new("agent resolution failed"));
        }
        test_agent(&request.agent_key, "demo-model")
    }

    fn build_run_config(
        &self,
        _request: &RunConfigResolutionRequest,
    ) -> Result<RunConfig, AppServerHostError> {
        if matches!(self.stage, FailureStage::RunConfig) {
            return Err(AppServerHostError::new("run config resolution failed"));
        }
        Ok(RunConfig::default())
    }

    fn list_models(
        &self,
        _request: &ModelListParams,
    ) -> Result<ModelListResponse, AppServerHostError> {
        if matches!(self.stage, FailureStage::Models) {
            return Err(AppServerHostError::new("model list failed"));
        }
        Ok(ModelListResponse { models: Vec::new() })
    }
}

#[derive(Default)]
struct ResolutionState {
    agent_requests: Mutex<Vec<AgentResolutionRequest>>,
    config_requests: Mutex<Vec<RunConfigResolutionRequest>>,
    runtime_metadata: Mutex<Vec<BTreeMap<String, Value>>>,
}

struct DynamicHost {
    state: Arc<ResolutionState>,
}

impl AppServerHost for DynamicHost {
    fn resolve_agent(&self, request: &AgentResolutionRequest) -> Result<Agent, AppServerHostError> {
        let call = {
            let mut requests = self.state.agent_requests.lock().expect("agent requests");
            requests.push(request.clone());
            requests.len()
        };
        let runtime_state = self.state.clone();
        Agent::builder(&request.agent_key)
            .dynamic_instructions(move |context, _agent| {
                runtime_state
                    .runtime_metadata
                    .lock()
                    .expect("runtime metadata")
                    .push(context.metadata.clone());
                "Answer the current turn, then finish.".to_string()
            })
            .model(ModelRef::named(format!("turn-model-{call}")))
            .build()
            .map_err(AppServerHostError::new)
    }

    fn build_run_config(
        &self,
        request: &RunConfigResolutionRequest,
    ) -> Result<RunConfig, AppServerHostError> {
        let call = {
            let mut requests = self.state.config_requests.lock().expect("config requests");
            requests.push(request.clone());
            requests.len()
        };
        let model = format!("turn-model-{call}");
        Ok(RunConfig::builder()
            .model(ModelRef::named(&model))
            .model_provider(ScriptedModelProvider::new(
                "scripted",
                model,
                vec![finish_response(&format!("resolved-turn-{call}"))],
            ))
            .workspace(request.cwd.clone().unwrap_or_else(|| PathBuf::from(".")))
            .metadata("host_resolution", json!(call))
            .build())
    }

    fn list_models(
        &self,
        _request: &ModelListParams,
    ) -> Result<ModelListResponse, AppServerHostError> {
        Ok(ModelListResponse { models: Vec::new() })
    }
}

fn test_agent(name: &str, model: &str) -> Result<Agent, AppServerHostError> {
    Agent::builder(name)
        .instructions("Answer the current turn, then finish.")
        .model(ModelRef::named(model))
        .build()
        .map_err(AppServerHostError::new)
}

fn scripted_runner(responses: Vec<LLMResponse>) -> Runner {
    Runner::builder()
        .model_provider(ScriptedModelProvider::new(
            "scripted",
            "fallback-model",
            responses,
        ))
        .workspace(".")
        .build()
        .expect("runner")
}

fn finish_response(message: &str) -> LLMResponse {
    let mut arguments = BTreeMap::new();
    arguments.insert("message".to_string(), json!(message));
    LLMResponse::with_tool_calls(
        message,
        vec![ToolCall::new(
            format!("finish-{message}"),
            "task_finish",
            arguments,
        )],
    )
}

fn assert_resolution_request(
    thread_id: &str,
    agent_key: &str,
    cwd: Option<&PathBuf>,
    metadata: &BTreeMap<String, Value>,
    request_id: i64,
) {
    assert_eq!(thread_id, "thread_1");
    assert_eq!(agent_key, "tenant-agent");
    assert_eq!(cwd, Some(&PathBuf::from("/tmp/tenant-workspace")));
    assert_eq!(metadata["tenant"], "acme");
    assert_eq!(metadata["role"], "turn");
    assert_eq!(metadata["requestId"], request_id);
    assert_eq!(metadata["thread_id"], "spoofed-turn-thread");
    assert_eq!(metadata["turn_id"], "spoofed-turn");
    assert_eq!(metadata["session_id"], "spoofed-turn-session");
    assert_eq!(metadata.len(), 6);
}

async fn initialize(
    processor: &mut MessageProcessor,
    outgoing: &mut mpsc::Receiver<OutgoingEnvelope>,
    connection_id: ConnectionId,
) {
    processor
        .process_message(
            connection_id,
            request(
                1,
                "initialize",
                json!({"clientInfo": {"name": "host-test"}}),
            ),
        )
        .await;
    let _ = expect_response(outgoing, RequestId::Integer(1)).await;
}

async fn start_thread(
    processor: &mut MessageProcessor,
    outgoing: &mut mpsc::Receiver<OutgoingEnvelope>,
    connection_id: ConnectionId,
) {
    processor
        .process_message(
            connection_id,
            request(2, "thread/start", json!({"agentKey": "tenant-agent"})),
        )
        .await;
    let _ = expect_response(outgoing, RequestId::Integer(2)).await;
    let _ = expect_notification(outgoing, "thread/started").await;
}

fn request(id: i64, method: &str, params: Value) -> JsonRpcMessage {
    JsonRpcMessage::Request(JsonRpcRequest {
        id: RequestId::Integer(id),
        method: method.to_string(),
        params: Some(params),
    })
}

async fn expect_response(
    outgoing: &mut mpsc::Receiver<OutgoingEnvelope>,
    expected_id: RequestId,
) -> Value {
    loop {
        let envelope = recv(outgoing).await;
        if let JsonRpcMessage::Response(response) = envelope.message {
            assert_eq!(response.id, expected_id);
            return response.result;
        }
    }
}

async fn expect_error(
    outgoing: &mut mpsc::Receiver<OutgoingEnvelope>,
    expected_id: RequestId,
) -> Value {
    loop {
        let envelope = recv(outgoing).await;
        if let JsonRpcMessage::Error(error) = envelope.message {
            assert_eq!(error.id, expected_id);
            return serde_json::to_value(error.error).expect("error value");
        }
    }
}

async fn expect_notification(
    outgoing: &mut mpsc::Receiver<OutgoingEnvelope>,
    expected_method: &str,
) -> Value {
    loop {
        let envelope = recv(outgoing).await;
        if let JsonRpcMessage::Notification(notification) = envelope.message {
            if notification.method == expected_method {
                return notification.params.unwrap_or(Value::Null);
            }
        }
    }
}

async fn recv(outgoing: &mut mpsc::Receiver<OutgoingEnvelope>) -> OutgoingEnvelope {
    tokio::time::timeout(Duration::from_secs(5), outgoing.recv())
        .await
        .expect("outgoing message timeout")
        .expect("outgoing message")
}
