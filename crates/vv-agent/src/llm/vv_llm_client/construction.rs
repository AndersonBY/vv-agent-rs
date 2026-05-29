use std::path::Path;
use std::sync::{Arc, Mutex};

use super::endpoints::EndpointChatClient;
use super::{EndpointClientSpec, NamedEndpointClientSpec, VvLlmClient};

impl VvLlmClient {
    pub fn new(
        backend: impl Into<String>,
        selected_model: impl Into<String>,
        model_id: impl Into<String>,
        chat_client: Box<dyn vv_llm::ChatClient>,
        timeout_seconds: f64,
    ) -> Self {
        let model_id = model_id.into();
        Self::new_with_named_endpoint_clients(
            backend,
            selected_model,
            model_id.clone(),
            vec![(model_id.clone(), model_id, chat_client)],
            timeout_seconds,
        )
    }

    pub fn new_with_endpoint_clients(
        backend: impl Into<String>,
        selected_model: impl Into<String>,
        model_id: impl Into<String>,
        endpoint_clients: Vec<EndpointClientSpec>,
        timeout_seconds: f64,
    ) -> Self {
        Self::new_with_named_endpoint_clients(
            backend,
            selected_model,
            model_id,
            endpoint_clients
                .into_iter()
                .map(|(model_id, chat_client)| (model_id.clone(), model_id, chat_client))
                .collect(),
            timeout_seconds,
        )
    }

    pub fn new_with_named_endpoint_clients(
        backend: impl Into<String>,
        selected_model: impl Into<String>,
        model_id: impl Into<String>,
        endpoint_clients: Vec<NamedEndpointClientSpec>,
        timeout_seconds: f64,
    ) -> Self {
        Self {
            backend: backend.into(),
            selected_model: selected_model.into(),
            model_id: model_id.into(),
            timeout_seconds,
            debug_dump_dir: None,
            max_retries_per_endpoint: 3,
            backoff_seconds: 2.0,
            randomize_endpoints: true,
            request_counter: Arc::new(Mutex::new(0)),
            endpoint_order_counter: Arc::new(Mutex::new(0)),
            preferred_endpoint_id: Arc::new(Mutex::new(None)),
            endpoint_clients: endpoint_clients
                .into_iter()
                .map(|(endpoint_id, model_id, chat_client)| {
                    EndpointChatClient::new(endpoint_id, model_id, chat_client)
                })
                .collect(),
        }
    }

    pub fn provider_name(&self) -> &'static str {
        self.endpoint_clients
            .first()
            .map(|endpoint| endpoint.chat_client.provider_name())
            .unwrap_or("unknown")
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn endpoint_count(&self) -> usize {
        self.endpoint_clients.len()
    }

    pub fn randomize_endpoints(&self) -> bool {
        self.randomize_endpoints
    }

    pub fn with_randomize_endpoints(mut self, randomize_endpoints: bool) -> Self {
        self.randomize_endpoints = randomize_endpoints;
        self
    }

    pub fn with_debug_dump_dir(mut self, debug_dump_dir: impl AsRef<Path>) -> Self {
        self.debug_dump_dir = Some(debug_dump_dir.as_ref().to_path_buf());
        self
    }

    pub fn with_retry_policy(
        mut self,
        max_retries_per_endpoint: usize,
        backoff_seconds: f64,
    ) -> Self {
        self.max_retries_per_endpoint = max_retries_per_endpoint.max(1);
        self.backoff_seconds = backoff_seconds.max(0.0);
        self
    }
}
