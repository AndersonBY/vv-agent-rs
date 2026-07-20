# Optional Output Validation

Contract `0.9.0` adds a default-off host extension that validates a completed
output and may make one tools-free repair request. It runs outside the normal
agent loop and never calls the primary model again.

## Rust API

Register callbacks on `AgentBuilder` and opt in explicitly:

```rust
use vv_agent::{
    Agent, ModelRef, ModelSettings, OutputValidationResult,
};

let agent = Agent::builder("validated-agent")
    .instructions("Return the answer.")
    .output_validation_enabled(true)
    .host_output_validator(|output, context| {
        if output.contains("answer") {
            OutputValidationResult::accept()
        } else {
            OutputValidationResult::reject(
                "answer_missing",
                Some(format!("{} returned no answer", context.agent_name)),
            )
        }
    })
    .output_repair(|request| {
        assert!(request.tools.is_empty());
        Ok(format!("answer: {}", request.invalid_output))
    })
    .output_validation_max_repairs(1)
    .output_repair_model(ModelRef::named("host-selected-repair-model"))
    .output_repair_model_settings(ModelSettings::builder().temperature(0.0).build())
    .build()?;
# Ok::<(), String>(())
```

Registering a callback does not enable it. The maximum repair count accepts
only `0` or `1`. `output_repair_model` and its settings are descriptors passed
to the host callback; the Runner does not resolve or invoke that model.

Rust exposes its normal public final output as a string, so
`host_output_validator` receives `&str`. When `output_type::<T>()` is present,
the Runner first verifies that the candidate can deserialize as `T`, and
`OutputValidationContext::output_type_name` identifies that declaration. A
repaired string must pass both typed deserialization and the same host
validator.

## Lifecycle And Failure

The Runner applies output guardrails and typed-output checks to create a
terminal candidate. The optional validator and one-shot repair then run before
session persistence, checkpoint finalization, and terminal-event emission.
The committed terminal is therefore either the validated success or a typed
validation failure.

Validation failure is returned as a normal `RunResult` with:

- `status() == AgentStatus::Failed`;
- `error_code() == Some("output_validation_failed")`;
- the invalid candidate retained as `partial_output()` when available;
- validation or repair diagnostics in the ordinary error text.

Exactly one terminal event records the final observation. A successful repair
produces `run_completed` with the repaired output; rejection produces
`run_failed`. Approval resume uses the same ordering. Terminal checkpoint
replay reuses the committed result and does not call the model, validator, or
repair callback again.

## Safety Boundary

`OutputValidationContext` contains only run identity, agent identity, and the
declared output type name. `OutputRepairRequest::tools` is always empty. The
extension cannot expand tool policy, infer a task type, inspect hidden scoring,
inject another agent cycle, or replace cancellation, budget exhaustion,
reconciliation, or operator-abort precedence.

Checkpoint v2 requires stable `output_validator` and `output_repair`
capability refs when those callbacks are enabled.

## Verification

```bash
cargo test -p vv-agent --test output_validation_contract
python3 scripts/contract_snapshot.py check --source ../vv-agent-contract
```
