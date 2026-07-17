mod helpers;
mod resume;
mod turns;

use helpers::{load_thread_resume_snapshot, parse_params, parse_params_or_default, store_error};

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::app_server::durable_resume::DurableTurnResumeProvider;
use crate::app_server::host::{AppServerHost, DefaultAppServerHost};
use crate::app_server::outgoing::{OutgoingEnvelope, OutgoingMessageSender};
use crate::app_server::protocol::{
    generate_app_server_json_schema_bundle, generate_app_server_typescript_bundle, AppClientInfo,
    AppServerCapabilities, AppServerError, AppServerErrorCode, ApprovalResolveParams,
    InitializeParams, InitializeResponse, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse, ModelListParams, SchemaExportResponse, ServerNotification,
    ThreadArchiveParams, ThreadArchiveResponse, ThreadClosedParams, ThreadListParams,
    ThreadListResponse, ThreadReadParams, ThreadReadResponse, ThreadResumeParams,
    ThreadResumeResponse, ThreadStartParams, ThreadStartResponse, ThreadStatus,
    ThreadStatusChangedParams, ThreadUnsubscribeParams, ThreadUnsubscribeResponse,
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
    host: Arc<dyn AppServerHost>,
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
    pub fn new(outgoing_capacity: usize) -> (Self, tokio::sync::mpsc::Receiver<OutgoingEnvelope>) {
        Self::new_with_host(outgoing_capacity, Arc::new(DefaultAppServerHost::default()))
    }

    pub fn new_with_host(
        outgoing_capacity: usize,
        host: Arc<dyn AppServerHost>,
    ) -> (Self, tokio::sync::mpsc::Receiver<OutgoingEnvelope>) {
        let (outgoing, rx) = OutgoingMessageSender::channel(outgoing_capacity);
        (
            Self {
                outgoing,
                host,
                connections: HashMap::new(),
                run_adapter: None,
                thread_state: ThreadStateManager::default(),
                request_queue: RequestSerializationQueue::default(),
            },
            rx,
        )
    }

    pub fn with_runtime(
        outgoing_capacity: usize,
        runner: Runner,
        agent: Agent,
        store: SqliteThreadStore,
    ) -> (Self, tokio::sync::mpsc::Receiver<OutgoingEnvelope>) {
        Self::with_runtime_and_approval_timeout(
            outgoing_capacity,
            runner,
            agent,
            store,
            Duration::from_secs(30),
        )
    }

    pub fn with_runtime_and_approval_timeout(
        outgoing_capacity: usize,
        runner: Runner,
        agent: Agent,
        store: SqliteThreadStore,
        approval_request_timeout: Duration,
    ) -> (Self, tokio::sync::mpsc::Receiver<OutgoingEnvelope>) {
        Self::with_host_and_approval_timeout(
            outgoing_capacity,
            runner,
            Arc::new(DefaultAppServerHost::from_agent(agent)),
            store,
            approval_request_timeout,
        )
    }

    pub fn with_host(
        outgoing_capacity: usize,
        runner: Runner,
        host: Arc<dyn AppServerHost>,
        store: SqliteThreadStore,
    ) -> (Self, tokio::sync::mpsc::Receiver<OutgoingEnvelope>) {
        Self::with_host_and_approval_timeout(
            outgoing_capacity,
            runner,
            host,
            store,
            Duration::from_secs(30),
        )
    }

    pub fn with_host_and_approval_timeout(
        outgoing_capacity: usize,
        runner: Runner,
        host: Arc<dyn AppServerHost>,
        store: SqliteThreadStore,
        approval_request_timeout: Duration,
    ) -> (Self, tokio::sync::mpsc::Receiver<OutgoingEnvelope>) {
        Self::with_host_options(
            outgoing_capacity,
            runner,
            host,
            store,
            approval_request_timeout,
            None,
        )
    }

    pub fn with_host_and_durable_resume_provider(
        outgoing_capacity: usize,
        runner: Runner,
        host: Arc<dyn AppServerHost>,
        store: SqliteThreadStore,
        provider: Arc<dyn DurableTurnResumeProvider>,
    ) -> (Self, tokio::sync::mpsc::Receiver<OutgoingEnvelope>) {
        Self::with_host_options(
            outgoing_capacity,
            runner,
            host,
            store,
            Duration::from_secs(30),
            Some(provider),
        )
    }

    pub fn with_runtime_and_durable_resume_provider(
        outgoing_capacity: usize,
        runner: Runner,
        agent: Agent,
        store: SqliteThreadStore,
        provider: Arc<dyn DurableTurnResumeProvider>,
    ) -> (Self, tokio::sync::mpsc::Receiver<OutgoingEnvelope>) {
        Self::with_host_and_durable_resume_provider(
            outgoing_capacity,
            runner,
            Arc::new(DefaultAppServerHost::from_agent(agent)),
            store,
            provider,
        )
    }

    fn with_host_options(
        outgoing_capacity: usize,
        runner: Runner,
        host: Arc<dyn AppServerHost>,
        store: SqliteThreadStore,
        approval_request_timeout: Duration,
        durable_resume_provider: Option<Arc<dyn DurableTurnResumeProvider>>,
    ) -> (Self, tokio::sync::mpsc::Receiver<OutgoingEnvelope>) {
        let (outgoing, rx) = OutgoingMessageSender::channel(outgoing_capacity);
        let thread_state = ThreadStateManager::default();
        let mut run_adapter = AppServerRunAdapter::with_host(
            runner,
            host.clone(),
            store,
            thread_state.clone(),
            outgoing.clone(),
        )
        .with_approval_request_timeout(approval_request_timeout);
        if let Some(provider) = durable_resume_provider {
            run_adapter = run_adapter.with_durable_resume_provider(provider);
        }
        (
            Self {
                outgoing,
                host,
                connections: HashMap::new(),
                run_adapter: Some(run_adapter),
                thread_state,
                request_queue: RequestSerializationQueue::default(),
            },
            rx,
        )
    }

    pub fn new_for_tests(
        outgoing_capacity: usize,
    ) -> (Self, tokio::sync::mpsc::Receiver<OutgoingEnvelope>) {
        Self::new(outgoing_capacity)
    }

    pub fn new_for_tests_with_runtime(
        outgoing_capacity: usize,
        runner: Runner,
        agent: Agent,
        store: SqliteThreadStore,
    ) -> (Self, tokio::sync::mpsc::Receiver<OutgoingEnvelope>) {
        Self::with_runtime(outgoing_capacity, runner, agent, store)
    }

    pub fn new_for_tests_with_runtime_and_approval_timeout(
        outgoing_capacity: usize,
        runner: Runner,
        agent: Agent,
        store: SqliteThreadStore,
        approval_request_timeout: Duration,
    ) -> (Self, tokio::sync::mpsc::Receiver<OutgoingEnvelope>) {
        Self::with_runtime_and_approval_timeout(
            outgoing_capacity,
            runner,
            agent,
            store,
            approval_request_timeout,
        )
    }

    pub fn new_for_tests_with_runtime_and_durable_resume_provider(
        outgoing_capacity: usize,
        runner: Runner,
        agent: Agent,
        store: SqliteThreadStore,
        provider: Arc<dyn DurableTurnResumeProvider>,
    ) -> (Self, tokio::sync::mpsc::Receiver<OutgoingEnvelope>) {
        Self::with_runtime_and_durable_resume_provider(
            outgoing_capacity,
            runner,
            agent,
            store,
            provider,
        )
    }

    pub fn outgoing(&self) -> &OutgoingMessageSender {
        &self.outgoing
    }

    pub fn connection_state(&self, connection_id: ConnectionId) -> Option<&ConnectionSessionState> {
        self.connections.get(&connection_id)
    }

    pub fn connection_ids(&self) -> Vec<ConnectionId> {
        self.connections.keys().copied().collect()
    }

    pub async fn disconnect_connection(&mut self, connection_id: ConnectionId) {
        self.connections.remove(&connection_id);
        self.thread_state
            .unsubscribe_connection(connection_id)
            .await;
        self.outgoing.unregister_connection(connection_id).await;
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
                let _ = self
                    .outgoing
                    .resolve_server_response(connection_id, response)
                    .await;
            }
            JsonRpcMessage::Error(error) => {
                let _ = self
                    .outgoing
                    .resolve_server_error(connection_id, error)
                    .await;
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
            "thread/unsubscribe" => {
                self.process_thread_unsubscribe(connection_id, request)
                    .await
            }
            "turn/start" => self.process_turn_start(connection_id, request).await,
            "turn/resume" => self.process_turn_resume(connection_id, request).await,
            "turn/interrupt" => self.process_turn_interrupt(connection_id, request).await,
            "turn/steer" => self.process_turn_steer(connection_id, request).await,
            "turn/followUp" => self.process_turn_follow_up(connection_id, request).await,
            "approval/resolve" => self.process_approval_resolve(connection_id, request).await,
            "model/list" => self.process_model_list(connection_id, request).await,
            "schema/export" => self.process_schema_export(connection_id, request).await,
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

    async fn process_schema_export(
        &mut self,
        connection_id: ConnectionId,
        request: JsonRpcRequest,
    ) {
        if request
            .params
            .as_ref()
            .is_some_and(|params| !params.as_object().is_some_and(serde_json::Map::is_empty))
        {
            let _ = self
                .outgoing
                .send_error(
                    connection_id,
                    request.id,
                    AppServerError::invalid_params("params must be an empty object"),
                )
                .await;
            return;
        }
        let json_schema = match generate_app_server_json_schema_bundle() {
            Ok(bundle) => bundle,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(
                        connection_id,
                        request.id,
                        AppServerError::internal(error.to_string()),
                    )
                    .await;
                return;
            }
        };
        let typescript = match generate_app_server_typescript_bundle() {
            Ok(bundle) => bundle,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(
                        connection_id,
                        request.id,
                        AppServerError::internal(error.to_string()),
                    )
                    .await;
                return;
            }
        };
        let result = serde_json::to_value(SchemaExportResponse {
            json_schema,
            typescript,
        })
        .expect("schema export response serializes");
        let _ = self
            .outgoing
            .send_response(connection_id, request.id, result)
            .await;
    }

    async fn process_model_list(&mut self, connection_id: ConnectionId, request: JsonRpcRequest) {
        let params = match parse_params_or_default::<ModelListParams>(request.params) {
            Ok(params) => params,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, error)
                    .await;
                return;
            }
        };
        let models = match self.host.list_models(&params) {
            Ok(models) => models,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(
                        connection_id,
                        request.id,
                        AppServerError::internal(error.to_string()),
                    )
                    .await;
                return;
            }
        };
        let result = serde_json::to_value(models).expect("model list response serializes");
        let _ = self
            .outgoing
            .send_response(connection_id, request.id, result)
            .await;
    }

    async fn process_approval_resolve(
        &mut self,
        connection_id: ConnectionId,
        request: JsonRpcRequest,
    ) {
        let params = match parse_params::<ApprovalResolveParams>(request.params) {
            Ok(params) => params,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, error)
                    .await;
                return;
            }
        };
        let decision = params.decision.as_wire();
        let resolved = self
            .outgoing
            .resolve_server_response_bound(
                connection_id,
                Some("approval/request"),
                Some(&params.thread_id),
                Some(&params.turn_id),
                JsonRpcResponse {
                    id: crate::app_server::protocol::RequestId::String(params.request_id.clone()),
                    result: json!({
                        "decision": decision,
                        "reason": params.reason,
                        "metadata": params.metadata,
                    }),
                },
            )
            .await;
        if !resolved {
            let _ = self
                .outgoing
                .send_error(
                    connection_id,
                    request.id,
                    AppServerError::invalid_params("Unknown approval request"),
                )
                .await;
            return;
        }
        let _ = self
            .outgoing
            .send_response(connection_id, request.id, json!({}))
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
        if params.client_info.name.trim().is_empty() {
            let _ = self
                .outgoing
                .send_error(
                    connection_id,
                    request.id,
                    AppServerError::invalid_params("clientInfo.name is required"),
                )
                .await;
            return;
        }
        state.initialized = true;
        state.ready_for_notifications = true;
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
        self.outgoing
            .mark_ready_for_notifications(connection_id)
            .await;

        let result = serde_json::to_value(InitializeResponse::new(
            AppServerCapabilities::for_runtime(self.run_adapter.is_some()),
        ))
        .expect("initialize response serializes");
        let _ = self
            .outgoing
            .send_response(connection_id, request.id, result)
            .await;
    }

    async fn process_thread_list(&mut self, connection_id: ConnectionId, request: JsonRpcRequest) {
        let params = match parse_params_or_default::<ThreadListParams>(request.params) {
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
            threads.retain(|thread| thread.archived_at.is_some() == archived);
        }
        for thread in &mut threads {
            if self.thread_state.is_closed(&thread.thread_id).await {
                thread.status = ThreadStatus::Closed;
            }
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
                        AppServerError::thread_not_found(),
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
        let archived = ThreadArchiveResponse {
            thread_id: params.thread_id.clone(),
            archived: true,
        };
        let result = serde_json::to_value(&archived).expect("thread archive response serializes");
        let _ = self
            .outgoing
            .send_response(connection_id, request.id, result)
            .await;
        let _ = self
            .outgoing
            .send_notification(connection_id, ServerNotification::ThreadArchived(archived))
            .await;
        let _ = self
            .outgoing
            .send_notification(
                connection_id,
                ServerNotification::ThreadStatusChanged(ThreadStatusChangedParams {
                    thread_id: params.thread_id,
                    status: ThreadStatus::Archived,
                }),
            )
            .await;
    }

    async fn process_thread_unsubscribe(
        &mut self,
        connection_id: ConnectionId,
        request: JsonRpcRequest,
    ) {
        let params = match parse_params::<ThreadUnsubscribeParams>(request.params) {
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
                        AppServerError::thread_not_found(),
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
        let closed = self
            .thread_state
            .unsubscribe(&params.thread_id, connection_id)
            .await;
        let result = serde_json::to_value(ThreadUnsubscribeResponse {
            thread_id: params.thread_id.clone(),
            subscribed: false,
            closed,
        })
        .expect("thread unsubscribe response serializes");
        let _ = self
            .outgoing
            .send_response(connection_id, request.id, result)
            .await;
        if closed {
            let _ = self
                .outgoing
                .send_notification(
                    connection_id,
                    ServerNotification::ThreadClosed(ThreadClosedParams {
                        thread_id: params.thread_id.clone(),
                    }),
                )
                .await;
            let _ = self
                .outgoing
                .send_notification(
                    connection_id,
                    ServerNotification::ThreadStatusChanged(ThreadStatusChangedParams {
                        thread_id: params.thread_id,
                        status: ThreadStatus::Closed,
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
        let params = match parse_params_or_default::<ThreadStartParams>(request.params) {
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
            .subscribe(thread.thread_id.clone(), connection_id)
            .await;
        let started = ThreadStartResponse::from_thread(&thread);
        let result = serde_json::to_value(&started).expect("thread start response serializes");
        let _ = self
            .outgoing
            .send_response(connection_id, request.id, result)
            .await;
        let _ = self
            .outgoing
            .send_notification(connection_id, ServerNotification::ThreadStarted(started))
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
        let Some(mut thread) = (match adapter.store().get_thread(&params.thread_id) {
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
                    AppServerError::thread_not_found(),
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
            if let Some(index) = items.iter().position(|item| item.item_id == after_item_id) {
                items = items.into_iter().skip(index + 1).collect();
            }
        }
        let turns = match adapter.store().list_turns(&params.thread_id) {
            Ok(turns) => turns,
            Err(error) => {
                let _ = self
                    .outgoing
                    .send_error(connection_id, request.id, store_error(error))
                    .await;
                return;
            }
        };
        if self.thread_state.is_closed(&params.thread_id).await {
            thread.status = ThreadStatus::Closed;
        }
        let result = serde_json::to_value(ThreadReadResponse {
            thread,
            turns,
            items,
        })
        .expect("thread read response serializes");
        let _ = self
            .outgoing
            .send_response(connection_id, request.id, result)
            .await;
    }
}
