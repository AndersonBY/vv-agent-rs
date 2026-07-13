use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::llm::{LlmClient, LlmRequest};
use crate::memory::SummaryCallback;
use crate::model::{ModelProvider, ModelRef};
use crate::types::Message;

type RoutedClient = (Arc<dyn LlmClient>, String);

pub(super) fn build_memory_summary_callback(
    provider: Arc<dyn ModelProvider>,
    default_backend: Option<String>,
    default_model: String,
) -> SummaryCallback {
    let clients = Arc::new(Mutex::new(BTreeMap::<(String, String), RoutedClient>::new()));
    Arc::new(move |prompt, backend, model| {
        let backend = backend
            .or(default_backend.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        let model = model.unwrap_or(&default_model).trim();
        if model.is_empty() {
            return None;
        }

        let key = (backend.to_string(), model.to_string());
        let cached = clients
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .cloned();
        let (client, request_model) = match cached {
            Some(cached) => cached,
            None => {
                let resolved = provider.resolve(&ModelRef::backend(backend, model)).ok()?;
                let client = provider.client(&resolved).ok()?;
                let routed = (client, resolved.selected_model.clone());
                clients
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(key, routed.clone());
                routed
            }
        };
        let response = client
            .complete(LlmRequest::new(request_model, vec![Message::user(prompt)]))
            .ok()?;
        let content = response.content.trim().to_string();
        (!content.is_empty()).then_some(content)
    })
}
