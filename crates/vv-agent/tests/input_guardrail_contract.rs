use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use vv_agent::{
    Agent, AgentStatus, GuardrailOutcome, InputGuardrail, LlmClient, ModelError, ModelProvider,
    ModelRef, NormalizedInput, ResolvedModelConfig, RunContext, RunEventPayload, Runner,
};

struct BlockInput;

impl InputGuardrail for BlockInput {
    fn check(
        &self,
        _context: &RunContext,
        _input: &NormalizedInput,
    ) -> GuardrailOutcome<NormalizedInput> {
        GuardrailOutcome::Block {
            message: "input is required".to_string(),
        }
    }
}

struct FailingProvider {
    calls: Arc<AtomicUsize>,
}

impl ModelProvider for FailingProvider {
    fn resolve(&self, _model: &ModelRef) -> Result<ResolvedModelConfig, ModelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(ModelError::Config("provider must not resolve".to_string()))
    }

    fn client(&self, _resolved: &ResolvedModelConfig) -> Result<Arc<dyn LlmClient>, ModelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(ModelError::Config(
            "provider must not build client".to_string(),
        ))
    }

    fn default_model_ref(&self) -> Option<ModelRef> {
        Some(ModelRef::named("blocked-model"))
    }
}

#[tokio::test]
async fn input_guardrail_blocks_before_provider_resolution() {
    let calls = Arc::new(AtomicUsize::new(0));
    let runner = Runner::builder()
        .model_provider(FailingProvider {
            calls: calls.clone(),
        })
        .workspace(".")
        .build()
        .expect("runner");
    let agent = Agent::builder("assistant")
        .instructions("Answer.")
        .input_guardrail(Arc::new(BlockInput))
        .build()
        .expect("agent");

    let result = runner.run(&agent, "   ").await.expect("blocked result");

    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(result.status(), AgentStatus::Failed);
    assert_eq!(result.final_output(), Some("input is required"));
    assert!(result.resolved_model().is_none());
    assert!(matches!(
        result.events().last().map(|event| event.payload()),
        Some(RunEventPayload::RunFailed { .. })
    ));
}
