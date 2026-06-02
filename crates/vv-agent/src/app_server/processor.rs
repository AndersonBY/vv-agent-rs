use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde_json::Value;

use crate::app_server::outgoing::{OutgoingEnvelope, OutgoingMessageSender};
use crate::app_server::protocol::{
    AppClientInfo, AppServerCapabilities, AppServerError, AppServerErrorCode, InitializeParams,
    InitializeResponse, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, ServerNotification,
    ThreadArchiveParams, ThreadArchiveResponse, ThreadArchivedParams, ThreadListParams,
    ThreadListResponse, ThreadReadParams, ThreadReadResponse, ThreadResumeParams,
    ThreadResumeResponse, ThreadStartParams, ThreadStartResponse, ThreadStartedParams,
    TurnInterruptParams, TurnInterruptResponse, TurnStartParams, TurnStartResponse,
};
use crate::app_server::request_serialization::{
    RequestSerializationQueue, RequestSerializationScope,
};
use crate::app_server::run_adapter::AppServerRunAdapter;
use crate::app_server::thread_state::ThreadStateManager;
use crate::app_server::thread_store::{SqliteThreadStore, ThreadStoreError};
use crate::app_server::transport::ConnectionId;
use crate::{Agent, Runner};

pub struct MessageProcessor {
    outgoing: OutgoingMessageSender,
    connections: HashMap<ConnectionId, ConnectionSessionState>,
    run_adapter: Option<AppServerRunAdapter>,
    thread_state: ThreadStateManager,
    request_queue: RequestSerializationQueue,
}

#[derive(Debug, Clone, Default)]
pub struct ConnectionSessionState {
    initialized: bool,
    ready_for_notifications: bool,
    client_info: Option<AppClientInfo>,
    experimental_api: bool,
    opt_out_notification_methods: HashSet<String>,
}

impl ConnectionSessionState {
    pub fn initialized(&self) -> bool {
        self.initialized
    }

    pub fn ready_for_notifications(&self) -> bool {
        self.ready_for_notifications
    }

    pub fn client_info(&self) -> Option<&AppClientInfo> {
        self.client_info.as_ref()
    }

    pub fn experimental_api(&self) -> bool {
        self.experimental_api
    }

    pub fn opt_out_notification_methods(&self) -> &HashSet<String> {
        &self.opt_out_notification_methods
    }
}

impl MessageProcessor {
    pub fn new_for_tests(
        outgoing_capacity: usize,
    ) -> (Self, tokio::sync::mpsc::Receiver<OutgoingEnvelope>) {
        let (outgoing, rx) = OutgoingMessageSender::channel(outgoing_capacity);
        (
            Self {
                outgoing,
                connections: HashMap::new(),
                run_adapter: None,
                thread_state: ThreadStateManager::default(),
                request_queue: RequestSerializationQueue::default(),
            },
            rx,
        )
    }

    pub fn new_for_tests_with_runtime(
        outgoing_capacity: usize,
        runner: Runner,
        agent: Agent,
        store: SqliteThreadStore,
    ) -> (Self, tokio::sync::mpsc::Receiver<OutgoingEnvelope>) {
        Self::new_for_tests_with_runtime_and_approval_timeout(
            outgoing_capacity,
            runner,
            agent,
            store,
            Duration::from_secs(30),
        )
    }

    pub fn new_for_tests_with_runtime_and_approval_timeout(
        outgoing_capacity: usize,
        runner: Runner,
        agent: Agent,
        store: SqliteThreadStore,
        approval_request_timeout: Duration,
    ) -> (Self, tokio::sync::mpsc::Receiver<OutgoingEnvelope>) {
        let (outgoing, rx) = OutgoingMessageSender::channel(outgoing_capacity);
        let thread_state = ThreadStateManager::default();
        let run_adapter =
            AppServerRunAdapter::new(runner, agent, store, thread_state.clone(), outgoing.clone())
                .with_approval_request_timeout(approval_request_timeout);
        (
            Self {
                outgoing,
                connections: HashMap::new(),
                run_adapter: Some(run_adapter),
                thread_state,
                request_queue: RequestSerializationQueue::default(),
            },
            rx,
        )
    }

    pub fn outgoing(&self) -> &OutgoingMessageSender {
        &self.outgoing
    }

    pub fn connection_state(&self, connection_id: ConnectionId) -> Option<&ConnectionSessionState> {
        self.connections.get(&connection_id)
    }

    pub async fn process_message(&mut self, connection_id: ConnectionId, message: JsonRpcMessage) {
        self.outgoing.register_connection(connection_id).await;
        match message {
            JsonRpcMessage::Request(request) => {
                self.process_request(connection_id, request).await;
            }
            JsonRpcMessage::Notification(notification) => {
                self.process_notification(connection_id, notification).await;
            }
            JsonRpcMessage::Response(response) => {
                let _ = self.outgoing.resolve_server_response(response).await;
            }
            JsonRpcMessage::Error(error) => {
                let _ = self.outgoing.resolve_server_error(error).await;
            }
        }
    }

    async fn process_request(&mut self, connection_id: ConnectionId, request: JsonRpcRequest) {
        if request.method == "initialize" {
            self.process_initialize(connection_id, request).await;
            return;
        }

        if !self
            .connections
            .get(&connection_id)
            .is_some_and(ConnectionSessionState::initialized)
        {
            let _ = self
                .outgoing
                .send_error(connection_id, request.id, AppServerError::not_initialized())
                .await;
            return;
        }

        if let Some(scope) =
            RequestSerializationScope::for_method(&request.method, request.params.as_ref())
        {
            let queue = self.request_queue.clone();
            queue
                .run(scope, async move {
                    self.process_initialized_request(connection_id, request)
                        .await;
                })
                .await;
            return;
        }

        self.process_initialized_request(connection_id, request)
            .await;
    }

    async fn process_initialized_request(
        &mut self,
        connection_id: ConnectionId,
        request: JsonRpcRequest,
    ) {
        match request.method.as_str() {
            "thread/start" => self.process_thread_start(connection_id, request).await,
            "thread/resume" => self.process_thread_resume(connection_id, request).await,
            "thread/read" => self.process_thread_read(connection_id, request).await,
            "thread/list" => self.process_thread_list(connection_id, request).await,
            "thread/archive" => self.process_thread_archive(connection_id, request).await,
            "turn/start" => self.process_turn_start(connection_id, request).await,
            "turn/interrupt" => self.process_turn_interrupt(connection_id, request).await,
            _ => {
                let _ = self
                    .outgoing
                    .send_error(
                        connection_id,
                        request.id,
                        AppServerError::new(
                            AppServerErrorCode::MethodNotFound,
                            format!("Method not found: {}", request.method),
                        )
                        .with_data(serde_json::json!({ "method": request.method })),
                    )
                    .await;
            }
        }
    }

    async fn process_thread_resume(
        &mut self,
        connection_id: ConnectionId,
        request: JsonRpcRequest,
    ) {
        let params = match parse_params::<ThreadResumeParams>(request.params) {
            Ok(params) => params,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, error)
                    .await;
                return;
            }
        };
        let Some(adapter) = self.run_adapter.clone() else {
            let _ = self
                .outgoing
                .send_error(
                    connection_id,
                    request.id,
                    AppServerError::internal("App Server runtime is not configured"),
                )
                .await;
            return;
        };
        let Some(thread) = (match adapter.store().get_thread(&params.thread_id) {
            Ok(thread) => thread,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, store_error(error))
                    .await;
                return;
            }
        }) else {
            let _ = self
                .outgoing
                .send_error(
                    connection_id,
                    request.id,
                    AppServerError::invalid_params("Unknown thread"),
                )
                .await;
            return;
        };
        let items = match adapter.store().replay_items(&params.thread_id) {
            Ok(items) => items,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, store_error(error))
                    .await;
                return;
            }
        };
        if params.subscribe {
            self.thread_state
                .subscribe(params.thread_id.clone(), connection_id)
                .await;
        }
        let active_turn = adapter.active_turn(&params.thread_id).await;
        let result = serde_json::to_value(ThreadResumeResponse {
            thread,
            items,
            active_turn,
        })
        .expect("thread resume response serializes");
        let _ = self
            .outgoing
            .send_response(connection_id, request.id, result)
            .await;
    }

    async fn process_initialize(&mut self, connection_id: ConnectionId, request: JsonRpcRequest) {
        let state = self.connections.entry(connection_id).or_default();
        if state.initialized {
            let _ = self
                .outgoing
                .send_error(
                    connection_id,
                    request.id,
                    AppServerError::already_initialized(),
                )
                .await;
            return;
        }

        let params = match parse_params::<InitializeParams>(request.params) {
            Ok(params) => params,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, error)
                    .await;
                return;
            }
        };
        state.initialized = true;
        state.client_info = Some(params.client_info);
        state.experimental_api = params.capabilities.experimental_api;
        state.opt_out_notification_methods = params
            .capabilities
            .opt_out_notification_methods
            .into_iter()
            .collect();
        self.outgoing
            .configure_connection(connection_id, state.opt_out_notification_methods.clone())
            .await;

        let result = serde_json::to_value(InitializeResponse::new(
            "vv-agent-rs",
            env!("CARGO_PKG_VERSION"),
            AppServerCapabilities::mvp(),
        ))
        .expect("initialize response serializes");
        let _ = self
            .outgoing
            .send_response(connection_id, request.id, result)
            .await;
    }

    async fn process_thread_list(&mut self, connection_id: ConnectionId, request: JsonRpcRequest) {
        let params = match parse_params::<ThreadListParams>(request.params) {
            Ok(params) => params,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, error)
                    .await;
                return;
            }
        };
        let Some(adapter) = self.run_adapter.clone() else {
            let _ = self
                .outgoing
                .send_error(
                    connection_id,
                    request.id,
                    AppServerError::internal("App Server runtime is not configured"),
                )
                .await;
            return;
        };
        let include_archived = params.include_archived || params.archived.unwrap_or(false);
        let mut threads = match adapter.store().list_threads(include_archived) {
            Ok(threads) => threads,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, store_error(error))
                    .await;
                return;
            }
        };
        if let Some(archived) = params.archived {
            threads.retain(|thread| thread.archived == archived);
        }
        let offset = params.offset.unwrap_or_default();
        let limit = params.limit.unwrap_or(threads.len());
        let threads = threads.into_iter().skip(offset).take(limit).collect();
        let result =
            serde_json::to_value(ThreadListResponse { threads }).expect("thread list serializes");
        let _ = self
            .outgoing
            .send_response(connection_id, request.id, result)
            .await;
    }

    async fn process_thread_archive(
        &mut self,
        connection_id: ConnectionId,
        request: JsonRpcRequest,
    ) {
        let params = match parse_params::<ThreadArchiveParams>(request.params) {
            Ok(params) => params,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, error)
                    .await;
                return;
            }
        };
        let Some(adapter) = self.run_adapter.clone() else {
            let _ = self
                .outgoing
                .send_error(
                    connection_id,
                    request.id,
                    AppServerError::internal("App Server runtime is not configured"),
                )
                .await;
            return;
        };
        match adapter.store().get_thread(&params.thread_id) {
            Ok(Some(_)) => {}
            Ok(None) => {
                let _ = self
                    .outgoing
                    .send_error(
                        connection_id,
                        request.id,
                        AppServerError::invalid_params("Unknown thread"),
                    )
                    .await;
                return;
            }
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, store_error(error))
                    .await;
                return;
            }
        }
        if let Err(error) = adapter.store().archive_thread(&params.thread_id) {
            let _ = self
                .outgoing
                .send_error(connection_id, request.id, store_error(error))
                .await;
            return;
        }
        self.thread_state
            .subscribe(params.thread_id.clone(), connection_id)
            .await;
        let result = serde_json::to_value(ThreadArchiveResponse {})
            .expect("thread archive response serializes");
        let _ = self
            .outgoing
            .send_response(connection_id, request.id, result)
            .await;
        let subscribers = self.thread_state.subscribers(&params.thread_id).await;
        for subscriber in subscribers {
            let _ = self
                .outgoing
                .send_notification(
                    subscriber,
                    ServerNotification::ThreadArchived(ThreadArchivedParams {
                        thread_id: params.thread_id.clone(),
                    }),
                )
                .await;
        }
    }

    async fn process_notification(
        &mut self,
        connection_id: ConnectionId,
        notification: JsonRpcNotification,
    ) {
        if notification.method != "initialized" {
            return;
        }
        let Some(state) = self.connections.get_mut(&connection_id) else {
            return;
        };
        if !state.initialized {
            return;
        }
        state.ready_for_notifications = true;
        self.outgoing
            .mark_ready_for_notifications(connection_id)
            .await;
    }

    async fn process_thread_start(&mut self, connection_id: ConnectionId, request: JsonRpcRequest) {
        let params = match parse_params::<ThreadStartParams>(request.params) {
            Ok(params) => params,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, error)
                    .await;
                return;
            }
        };
        let Some(adapter) = self.run_adapter.clone() else {
            let _ = self
                .outgoing
                .send_error(
                    connection_id,
                    request.id,
                    AppServerError::internal("App Server runtime is not configured"),
                )
                .await;
            return;
        };
        let thread = match adapter.store().create_thread(params) {
            Ok(thread) => thread,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, store_error(error))
                    .await;
                return;
            }
        };
        self.thread_state
            .subscribe(thread.id.clone(), connection_id)
            .await;
        let result = serde_json::to_value(ThreadStartResponse {
            thread: thread.clone(),
        })
        .expect("thread start response serializes");
        let _ = self
            .outgoing
            .send_response(connection_id, request.id, result)
            .await;
        let _ = self
            .outgoing
            .send_notification(
                connection_id,
                ServerNotification::ThreadStarted(ThreadStartedParams { thread }),
            )
            .await;
    }

    async fn process_thread_read(&mut self, connection_id: ConnectionId, request: JsonRpcRequest) {
        let params = match parse_params::<ThreadReadParams>(request.params) {
            Ok(params) => params,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, error)
                    .await;
                return;
            }
        };
        let Some(adapter) = self.run_adapter.clone() else {
            let _ = self
                .outgoing
                .send_error(
                    connection_id,
                    request.id,
                    AppServerError::internal("App Server runtime is not configured"),
                )
                .await;
            return;
        };
        let Some(thread) = (match adapter.store().get_thread(&params.thread_id) {
            Ok(thread) => thread,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, store_error(error))
                    .await;
                return;
            }
        }) else {
            let _ = self
                .outgoing
                .send_error(
                    connection_id,
                    request.id,
                    AppServerError::invalid_params("Unknown thread"),
                )
                .await;
            return;
        };
        let mut items = match adapter.store().replay_items(&params.thread_id) {
            Ok(items) => items,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, store_error(error))
                    .await;
                return;
            }
        };
        if let Some(after_item_id) = params.after_item_id {
            if let Some(index) = items.iter().position(|item| item.id == after_item_id) {
                items = items.into_iter().skip(index + 1).collect();
            }
        }
        let active_turn = adapter.active_turn(&params.thread_id).await;
        let result = serde_json::to_value(ThreadReadResponse {
            thread,
            items,
            active_turn,
        })
        .expect("thread read response serializes");
        let _ = self
            .outgoing
            .send_response(connection_id, request.id, result)
            .await;
    }

    async fn process_turn_start(&mut self, connection_id: ConnectionId, request: JsonRpcRequest) {
        let params = match parse_params::<TurnStartParams>(request.params) {
            Ok(params) => params,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, error)
                    .await;
                return;
            }
        };
        let Some(adapter) = self.run_adapter.clone() else {
            let _ = self
                .outgoing
                .send_error(
                    connection_id,
                    request.id,
                    AppServerError::internal("App Server runtime is not configured"),
                )
                .await;
            return;
        };
        self.thread_state
            .subscribe(params.thread_id.clone(), connection_id)
            .await;
        let turn = match adapter.start_turn(params).await {
            Ok(turn) => turn,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, error)
                    .await;
                return;
            }
        };
        let result = serde_json::to_value(TurnStartResponse { turn: turn.clone() })
            .expect("turn start response serializes");
        let _ = self
            .outgoing
            .send_response(connection_id, request.id, result)
            .await;
        let _ = adapter.notify_turn_started(&turn).await;
        adapter
            .spawn_event_forwarding(turn.thread_id.clone(), turn.id.clone())
            .await;
    }

    async fn process_turn_interrupt(
        &mut self,
        connection_id: ConnectionId,
        request: JsonRpcRequest,
    ) {
        let params = match parse_params::<TurnInterruptParams>(request.params) {
            Ok(params) => params,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, error)
                    .await;
                return;
            }
        };
        let Some(adapter) = self.run_adapter.clone() else {
            let _ = self
                .outgoing
                .send_error(
                    connection_id,
                    request.id,
                    AppServerError::internal("App Server runtime is not configured"),
                )
                .await;
            return;
        };
        match adapter
            .interrupt_turn(&params.thread_id, &params.turn_id)
            .await
        {
            Ok(outcome) => {
                let result = serde_json::to_value(TurnInterruptResponse {})
                    .expect("turn interrupt response serializes");
                let _ = self
                    .outgoing
                    .send_response(connection_id, request.id, result)
                    .await;
                if let Some(resolved_approval) = outcome.approval_resolved {
                    let _ = adapter.notify_approval_resolved(resolved_approval).await;
                }
                if let Some(completed_turn) = outcome.completed_turn {
                    let _ = adapter.notify_turn_completed(completed_turn).await;
                }
            }
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, error)
                    .await;
            }
        }
    }
}

fn parse_params<T: serde::de::DeserializeOwned>(
    params: Option<Value>,
) -> Result<T, AppServerError> {
    let params = params.ok_or_else(|| AppServerError::invalid_params("Missing params"))?;
    serde_json::from_value(params)
        .map_err(|error| AppServerError::invalid_params(error.to_string()))
}

fn store_error(error: ThreadStoreError) -> AppServerError {
    AppServerError::internal(error.to_string())
}
