use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::budget::{BudgetExhaustion, BudgetUsageSnapshot};
use crate::checkpoint::CheckpointError;
use crate::events::{DiagnosticLevel, RunEvent};
use crate::llm::{LlmClient, LlmRequest};
use crate::memory::{
    RuntimeMemoryCallback, RuntimeMemoryCallbackError, RuntimeMemoryCallbacks,
    SessionMemoryOutputDiagnostic,
};
use crate::model::{ModelProvider, ModelRef};
use crate::runtime::model_calls::{ModelCallCoordinator, ModelCallDispatchRequest};
use crate::runtime::{CancellationToken, RunEventHandler};
use crate::types::{AgentResult, Message, ModelCallOperation};

use super::super::checkpoint::{CheckpointCoordinator, CheckpointModelDispatch};

type BudgetSnapshotProvider = Arc<dyn Fn() -> Option<BudgetUsageSnapshot> + Send + Sync + 'static>;
type BudgetExhaustionProvider = Arc<dyn Fn() -> Option<BudgetExhaustion> + Send + Sync + 'static>;

#[derive(Debug)]
pub(crate) enum MemoryInferenceControl {
    BudgetExhausted(BudgetExhaustion),
    Cancelled,
    Interrupted(Box<AgentResult>),
    CheckpointFailed(CheckpointError),
}

#[derive(Clone)]
struct RoutedClient {
    client: Arc<dyn LlmClient>,
    request_model: String,
    backend: String,
    model: String,
}

struct MemoryModelRouter {
    provider: Option<Arc<dyn ModelProvider>>,
    direct_client: Arc<dyn LlmClient>,
    default_backend: String,
    default_model: String,
    clients: Mutex<BTreeMap<(String, String), RoutedClient>>,
}

impl MemoryModelRouter {
    fn resolve(&self, backend: Option<&str>, model: Option<&str>) -> Option<RoutedClient> {
        let requested_backend = backend.map(str::trim).filter(|value| !value.is_empty());
        let requested_model = model
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(&self.default_model);
        if requested_model.is_empty() {
            return None;
        }
        let key = (
            requested_backend.unwrap_or("").to_string(),
            requested_model.to_string(),
        );
        if let Some(cached) = self
            .clients
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .cloned()
        {
            return Some(cached);
        }

        let uses_default_route = requested_model == self.default_model
            && requested_backend.is_none_or(|backend| backend == self.default_backend);
        let routed = match (uses_default_route, self.provider.as_ref()) {
            (true, _) => RoutedClient {
                client: self.direct_client.clone(),
                request_model: requested_model.to_string(),
                backend: self.default_backend.clone(),
                model: self.default_model.clone(),
            },
            (false, Some(provider)) => {
                let model_ref = match requested_backend {
                    Some(backend) => ModelRef::backend(backend, requested_model),
                    None => ModelRef::named(requested_model),
                };
                let resolved = provider.resolve(&model_ref).ok()?;
                RoutedClient {
                    client: provider.client(&resolved).ok()?,
                    request_model: resolved.selected_model.clone(),
                    backend: resolved.backend.clone(),
                    model: resolved.selected_model,
                }
            }
            (false, None) if requested_backend.is_none() => RoutedClient {
                client: self.direct_client.clone(),
                request_model: requested_model.to_string(),
                backend: self.default_backend.clone(),
                model: self.default_model.clone(),
            },
            (false, None) => return None,
        };
        self.clients
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, routed.clone());
        Some(routed)
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_runtime_memory_callbacks(
    provider: Option<Arc<dyn ModelProvider>>,
    direct_client: Arc<dyn LlmClient>,
    default_backend: String,
    default_model: String,
    checkpoint: CheckpointCoordinator,
    accounting: ModelCallCoordinator,
    budget_snapshot: BudgetSnapshotProvider,
    budget_exhaustion: BudgetExhaustionProvider,
    cancellation_token: Option<CancellationToken>,
    run_id: String,
    trace_id: String,
    agent_name: String,
    session_id: Option<String>,
    parent_run_id: Option<String>,
    event_handler: Option<RunEventHandler>,
) -> RuntimeMemoryCallbacks {
    let diagnostic_default_backend = default_backend.clone();
    let diagnostic_default_model = default_model.clone();
    let router = Arc::new(MemoryModelRouter {
        provider,
        direct_client,
        default_backend,
        default_model,
        clients: Mutex::new(BTreeMap::new()),
    });
    let session_memory_diagnostic = event_handler.map(|event_handler| {
        Arc::new(move |diagnostic: &SessionMemoryOutputDiagnostic| {
            let mut event = RunEvent::diagnostic(
                &run_id,
                &trace_id,
                &agent_name,
                Some(diagnostic.cycle_index),
                DiagnosticLevel::Warning,
                "session_memory_output_invalid",
                serde_json::Map::from_iter([
                    (
                        "reason".to_string(),
                        serde_json::Value::String(diagnostic.reason.as_str().to_string()),
                    ),
                    (
                        "backend".to_string(),
                        serde_json::Value::String(
                            diagnostic
                                .backend
                                .clone()
                                .filter(|value| !value.trim().is_empty())
                                .unwrap_or_else(|| diagnostic_default_backend.clone()),
                        ),
                    ),
                    (
                        "model".to_string(),
                        serde_json::Value::String(
                            diagnostic
                                .model
                                .clone()
                                .filter(|value| !value.trim().is_empty())
                                .unwrap_or_else(|| diagnostic_default_model.clone()),
                        ),
                    ),
                ]),
            );
            if let Some(session_id) = session_id.as_deref() {
                event = event.with_session_id(session_id);
            }
            if let Some(parent_run_id) = parent_run_id.as_deref() {
                event = event.with_parent_run_id(parent_run_id);
            }
            event_handler(&event);
        }) as crate::memory::SessionMemoryDiagnosticCallback
    });
    RuntimeMemoryCallbacks {
        session_memory: Some(build_runtime_memory_callback(
            router.clone(),
            checkpoint.clone(),
            accounting.clone(),
            budget_snapshot.clone(),
            budget_exhaustion.clone(),
            cancellation_token.clone(),
            ModelCallOperation::SessionMemory,
            "session",
        )),
        memory_compaction: Some(build_runtime_memory_callback(
            router,
            checkpoint,
            accounting,
            budget_snapshot,
            budget_exhaustion,
            cancellation_token,
            ModelCallOperation::MemoryCompaction,
            "compaction",
        )),
        session_memory_diagnostic,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_runtime_memory_callback(
    router: Arc<MemoryModelRouter>,
    checkpoint: CheckpointCoordinator,
    accounting: ModelCallCoordinator,
    budget_snapshot: BudgetSnapshotProvider,
    budget_exhaustion: BudgetExhaustionProvider,
    cancellation_token: Option<CancellationToken>,
    operation: ModelCallOperation,
    operation_slot: &'static str,
) -> RuntimeMemoryCallback {
    Arc::new(move |prompt, backend, model, cycle_index| {
        if cancellation_token
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(control_error(MemoryInferenceControl::Cancelled));
        }
        let Some(routed) = router.resolve(backend, model) else {
            return Ok(None);
        };
        let request = LlmRequest::new(routed.request_model.clone(), vec![Message::user(prompt)]);
        let client = routed.client.clone();
        let dispatch = checkpoint.dispatch_model(
            ModelCallDispatchRequest {
                cycle_index,
                operation_slot,
                operation,
                backend: &routed.backend,
                model: &routed.model,
                request: &request,
                accounting: &accounting,
            },
            || budget_snapshot(),
            move |request| client.complete(request),
        );
        match dispatch {
            CheckpointModelDispatch::Continue(result) => match *result {
                Ok(dispatch) => {
                    if let Some(exhaustion) =
                        dispatch.budget_exhaustion.or_else(|| budget_exhaustion())
                    {
                        return Err(control_error(MemoryInferenceControl::BudgetExhausted(
                            exhaustion,
                        )));
                    }
                    if cancellation_token
                        .as_ref()
                        .is_some_and(CancellationToken::is_cancelled)
                    {
                        return Err(control_error(MemoryInferenceControl::Cancelled));
                    }
                    Ok(Some(dispatch.response.content))
                }
                Err(_) => {
                    if let Some(exhaustion) = budget_exhaustion() {
                        return Err(control_error(MemoryInferenceControl::BudgetExhausted(
                            exhaustion,
                        )));
                    }
                    if cancellation_token
                        .as_ref()
                        .is_some_and(CancellationToken::is_cancelled)
                    {
                        return Err(control_error(MemoryInferenceControl::Cancelled));
                    }
                    Ok(None)
                }
            },
            CheckpointModelDispatch::Interrupted(result) => {
                Err(control_error(MemoryInferenceControl::Interrupted(result)))
            }
            CheckpointModelDispatch::Failed(error) => Err(control_error(
                MemoryInferenceControl::CheckpointFailed(error),
            )),
        }
    })
}

fn control_error(control: MemoryInferenceControl) -> RuntimeMemoryCallbackError {
    RuntimeMemoryCallbackError::new(control)
}

pub(crate) fn decode_control(
    error: RuntimeMemoryCallbackError,
) -> Result<MemoryInferenceControl, RuntimeMemoryCallbackError> {
    error.downcast::<MemoryInferenceControl>()
}
