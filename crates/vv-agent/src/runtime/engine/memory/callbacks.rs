use std::sync::Arc;

use crate::llm::{LlmClient, LlmRequest};
use crate::memory::{SessionMemoryExtractionCallback, SummaryCallback};
use crate::types::Message;

pub(super) fn build_memory_summary_callback<C>(client: C, default_model: String) -> SummaryCallback
where
    C: LlmClient + Clone + 'static,
{
    Arc::new(move |prompt, _backend, model| {
        let request_model = model.unwrap_or(&default_model).to_string();
        let response = client
            .clone()
            .complete(LlmRequest::new(request_model, vec![Message::user(prompt)]))
            .ok()?;
        let content = response.content.trim().to_string();
        (!content.is_empty()).then_some(content)
    })
}

pub(super) fn build_session_memory_extraction_callback<C>(
    client: C,
) -> SessionMemoryExtractionCallback
where
    C: LlmClient + Clone + 'static,
{
    Arc::new(move |prompt, _backend, model| {
        let request = LlmRequest::new(
            model.unwrap_or_default(),
            vec![Message::user(prompt.to_string())],
        );
        client
            .complete(request)
            .ok()
            .map(|response| response.content.trim().to_string())
            .filter(|content| !content.is_empty())
    })
}
