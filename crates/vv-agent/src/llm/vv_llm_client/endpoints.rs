use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::types::LLMResponse;

use super::VvLlmClient;

#[derive(Clone)]
pub(super) struct EndpointChatClient {
    pub(super) endpoint_id: String,
    pub(super) model_id: String,
    pub(super) chat_client: Arc<dyn vv_llm::ChatClient>,
}

impl EndpointChatClient {
    pub(super) fn new(
        endpoint_id: String,
        model_id: String,
        chat_client: Box<dyn vv_llm::ChatClient>,
    ) -> Self {
        Self {
            endpoint_id,
            model_id,
            chat_client: Arc::from(chat_client),
        }
    }
}

impl VvLlmClient {
    pub(super) fn effective_model_for_endpoint(
        &self,
        requested_model: &str,
        endpoint: &EndpointChatClient,
    ) -> String {
        let requested_model = requested_model.trim();
        if requested_model.is_empty()
            || requested_model == self.model_id
            || requested_model == self.selected_model
        {
            endpoint.model_id.clone()
        } else {
            requested_model.to_string()
        }
    }

    pub(super) fn ordered_endpoint_clients(&self) -> Vec<EndpointChatClient> {
        let mut endpoints = self.endpoint_clients.clone();
        let preferred_endpoint_id = self
            .preferred_endpoint_id
            .lock()
            .ok()
            .and_then(|preferred| preferred.clone());
        if let Some(preferred_endpoint_id) = preferred_endpoint_id {
            if let Some(index) = endpoints
                .iter()
                .position(|endpoint| endpoint.endpoint_id == preferred_endpoint_id)
            {
                let preferred = endpoints.remove(index);
                if self.randomize_endpoints {
                    shuffle_endpoint_clients(&mut endpoints, self.next_endpoint_shuffle_seed());
                }
                endpoints.insert(0, preferred);
                return endpoints;
            }
        }
        if self.randomize_endpoints {
            shuffle_endpoint_clients(&mut endpoints, self.next_endpoint_shuffle_seed());
        }
        endpoints
    }

    pub(super) fn remember_preferred_endpoint(&self, endpoint_id: &str) {
        if let Ok(mut preferred_endpoint_id) = self.preferred_endpoint_id.lock() {
            *preferred_endpoint_id = Some(endpoint_id.to_string());
        }
    }

    pub(super) fn dump_request_messages(&self, messages: &[vv_llm::Message], model_name: &str) {
        let Some(dump_dir) = &self.debug_dump_dir else {
            return;
        };
        let Ok(mut request_counter) = self.request_counter.lock() else {
            return;
        };
        *request_counter += 1;
        let request_index = *request_counter;

        let _ = std::fs::create_dir_all(dump_dir);
        let filename = format!(
            "request_{request_index:03}_{}.json",
            safe_model_filename(model_name)
        );
        let payload = serde_json::json!({
            "request_index": request_index,
            "model": model_name,
            "message_count": messages.len(),
            "messages": messages,
        });
        if let Ok(content) = serde_json::to_string_pretty(&payload) {
            let _ = std::fs::write(dump_dir.join(filename), content);
        }
    }

    pub(super) fn sleep_backoff(&self, attempt: usize) {
        if self.backoff_seconds <= 0.0 {
            return;
        }
        std::thread::sleep(Duration::from_secs_f64(
            self.backoff_seconds * attempt as f64,
        ));
    }

    fn next_endpoint_shuffle_seed(&self) -> u64 {
        let counter = self
            .endpoint_order_counter
            .lock()
            .map(|mut counter| {
                *counter = counter.wrapping_add(1);
                *counter
            })
            .unwrap_or(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0);
        nanos ^ counter.rotate_left(17)
    }
}

pub(super) fn annotate_endpoint_response(
    response: &mut LLMResponse,
    endpoint: &EndpointChatClient,
    model_id: &str,
    stream_mode: bool,
) {
    response.raw.insert(
        "used_endpoint_id".to_string(),
        Value::String(endpoint.endpoint_id.clone()),
    );
    response.raw.insert(
        "used_model_id".to_string(),
        Value::String(model_id.to_string()),
    );
    response
        .raw
        .insert("stream_mode".to_string(), Value::Bool(stream_mode));
}

fn shuffle_endpoint_clients(endpoints: &mut [EndpointChatClient], seed: u64) {
    if endpoints.len() < 2 {
        return;
    }

    let mut state = seed ^ ((endpoints.len() as u64) << 32) ^ 0x9E37_79B9_7F4A_7C15;
    for index in (1..endpoints.len()).rev() {
        let swap_index = (next_shuffle_u64(&mut state) as usize) % (index + 1);
        endpoints.swap(index, swap_index);
    }
}

fn next_shuffle_u64(state: &mut u64) -> u64 {
    let mut value = if *state == 0 {
        0xA076_1D64_78BD_642F
    } else {
        *state
    };
    value ^= value << 7;
    value ^= value >> 9;
    value ^= value << 8;
    *state = value;
    value
}

fn safe_model_filename(model_name: &str) -> String {
    let safe = model_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let safe = safe.trim_matches('_');
    if safe.is_empty() {
        "model".to_string()
    } else {
        safe.to_string()
    }
}
