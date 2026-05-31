use crate::context::RunContext;
use crate::runner::NormalizedInput;
use crate::types::AgentResult;

pub trait InputGuardrail: Send + Sync {
    fn check(
        &self,
        _ctx: &RunContext,
        input: &NormalizedInput,
    ) -> GuardrailOutcome<NormalizedInput> {
        GuardrailOutcome::Allow(input.clone())
    }
}

pub trait OutputGuardrail: Send + Sync {
    fn check(&self, _ctx: &RunContext, output: &AgentResult) -> GuardrailOutcome<AgentResult> {
        GuardrailOutcome::Allow(output.clone())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum GuardrailOutcome<T> {
    Allow(T),
    Block { message: String },
    RequireApproval { message: String },
}
