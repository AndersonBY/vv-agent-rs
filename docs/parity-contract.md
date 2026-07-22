# Rust Contract Integration

`vv-agent-rs` implements the Rust side of the canonical contract published by
[`AndersonBY/vv-agent-contract`](https://github.com/AndersonBY/vv-agent-contract).
The normative behavior and change workflow no longer live in this repository.

## Pinned Contract

`contract.lock.json` is the machine-readable adoption record. It pins:

- semantic contract version;
- exact central Git revision;
- immutable release artifact URL and SHA-256;
- local vendored snapshot path;
- canonical `SHA256SUMS` digest.

`crates/vv-agent/tests/fixtures/parity/` is generated from that release. It is
committed for offline and reproducible tests, but it is not an editable source
of truth.

The current lock selects contract `2.0.0` at revision
`8ef7153e9b1f26b90a9fad85bbfcb4642d6462fa`. The central support matrix is
`verified`. Recording run
[`29934473634`](https://github.com/AndersonBY/vv-agent-contract/actions/runs/29934473634)
passed against Python revision
`64743760634fa70c76bf523bf4b51601713ccfb0` and Rust revision
`91a53cd1be9ad560f99c93c4437bc2d830271a09`. This document maps the Rust
producers; the central matrix remains the authoritative current verification
record.

## Required Reading

For shared public, model-visible, runtime, persistence, or wire changes, read:

1. `contract.lock.json` in this repository;
2. `../vv-agent-contract/AGENTS.md`;
3. `../vv-agent-contract/docs/parity-contract.md`;
4. `../vv-agent-contract/docs/change-workflow.md`;
5. sibling `../vv-agent/docs/parity-contract.md`.

If the sibling checkout is unavailable, use the exact repository and revision
from the lock. Do not infer the current contract from a floating `main` branch.

## Snapshot Commands

Offline verification of the committed snapshot:

```bash
python3 scripts/contract_snapshot.py check
```

Stronger verification against the sibling canonical checkout:

```bash
python3 scripts/contract_snapshot.py check --source ../vv-agent-contract
```

Synchronization is allowed only after the canonical version is committed and
its deterministic release zip exists:

```bash
python3 scripts/contract_snapshot.py sync \
  --source ../vv-agent-contract \
  --artifact /path/to/vv-agent-contract-<version>.zip \
  --artifact-url https://github.com/AndersonBY/vv-agent-contract/releases/download/v<version>/vv-agent-contract-<version>.zip
```

Never repair a contract failure by editing a file under
`crates/vv-agent/tests/fixtures/parity/` or changing only a digest.

## Rust Producer Map

| Contract surface | Rust producer or evidence |
| --- | --- |
| Public API inventory | `crates/vv-agent/src/lib.rs`, `crates/vv-agent/tests/parity_evidence_manifests.rs` |
| System prompt | `crates/vv-agent/src/prompt/`, `crates/vv-agent/tests/prompt_public_api.rs` |
| Built-in tool specification | `crates/vv-agent/src/tools/`, `crates/vv-agent/tests/tool_schema_contract.rs` |
| Typed tool declaration and public propagation | `crates/vv-agent/src/tools/metadata.rs`, `crates/vv-agent/src/tools/function.rs`, `crates/vv-agent/src/tools/public_tool.rs`, `crates/vv-agent/src/tools/base/spec.rs`, `crates/vv-agent/src/tools/executor.rs`, `crates/vv-agent/src/tools/registry/mod.rs`; `crates/vv-agent/tests/tool_metadata_contract.rs`, `crates/vv-agent/tests/parity_evidence_manifests.rs`, `crates/vv-agent/tests/tool_orchestrator.rs`, `crates/vv-agent/tests/tool_schema_contract.rs` |
| Metadata denial policy and delegation | `crates/vv-agent/src/tools/policy.rs`, `crates/vv-agent/src/runner/support.rs`, `crates/vv-agent/src/runtime/tool_planner.rs`, `crates/vv-agent/src/runtime/sub_agents/`, `crates/vv-agent/src/runner/handoff.rs`, `crates/vv-agent/src/runtime/backends/distributed/`; `crates/vv-agent/tests/runner_tool_policy.rs`, `crates/vv-agent/tests/configured_sub_agent_parity.rs`, `crates/vv-agent/tests/agent_tool_contract.rs`, `crates/vv-agent/tests/handoff_contract.rs`, `crates/vv-agent/tests/distributed_checkpoint.rs` |
| Agent, Runner, result, live control | `crates/vv-agent/src/agent.rs`, `crates/vv-agent/src/runner/`, `crates/vv-agent/src/run_handle.rs` |
| Optional output validation and repair | `crates/vv-agent/src/output_validation.rs`, `crates/vv-agent/src/agent.rs`, `crates/vv-agent/src/runner/`, `crates/vv-agent/tests/output_validation_contract.rs` |
| Delegation and background tasks | `crates/vv-agent/src/tools/background_agent_task.rs`, `crates/vv-agent/src/handoffs.rs`, `crates/vv-agent/src/runtime/sub_agents/` |
| Sessions and stores | `crates/vv-agent/src/sessions.rs`, `crates/vv-agent/src/runtime/stores/`, `crates/vv-agent/tests/session_store_parity.rs` |
| Events and tracing | `crates/vv-agent/src/events.rs`, `crates/vv-agent/src/event_store.rs`, `crates/vv-agent/src/tracing.rs` |
| Tool planned/started/completed lifecycle | `crates/vv-agent/src/tools/orchestrator.rs`, `crates/vv-agent/src/runtime/engine/tool_batch.rs`, `crates/vv-agent/src/events.rs`, `crates/vv-agent/src/events/wire.rs`, `crates/vv-agent/src/runner/event_stream.rs`, `crates/vv-agent/src/runner/resume.rs`; `crates/vv-agent/tests/runtime_cycle/hooks.rs`, `crates/vv-agent/tests/runner_producer_parity.rs`, `crates/vv-agent/tests/run_events_contract.rs`, `crates/vv-agent/tests/run_event_validation.rs`, `crates/vv-agent/tests/approval_resume_completion.rs` |
| Model stream projection | `crates/vv-agent/src/events/`, `crates/vv-agent/src/runner/event_stream/stream_projection.rs`, `crates/vv-agent/src/runner/run_single.rs`, `crates/vv-agent/src/runtime/sub_agents/events.rs`, `crates/vv-agent/src/app_server/protocol/item.rs`, `crates/vv-agent/tests/runner_producer_parity.rs` |
| Token and cache usage | `crates/vv-agent/src/types/token_usage.rs`, `crates/vv-agent/src/runtime/token_usage.rs`, `crates/vv-agent/src/llm/vv_llm_client/`, `crates/vv-agent/tests/token_usage.rs` |
| Assistant reasoning history | `crates/vv-agent/src/memory/message_sanitizer.rs`, `crates/vv-agent/src/llm/vv_llm_client/`, `crates/vv-agent/tests/message_sanitizer.rs`, `crates/vv-agent/tests/completion_policy_contract.rs` |
| Memory capacity and compaction lifecycle | `crates/vv-agent/src/config.rs`, `crates/vv-agent/src/memory/manager/`, `crates/vv-agent/src/runtime/engine/memory.rs`, `crates/vv-agent/src/runner/event_stream.rs`, `crates/vv-agent/src/events/`; `crates/vv-agent/tests/memory_lifecycle_contract.rs`, `crates/vv-agent/tests/runtime_cycle/microcompact.rs`, `crates/vv-agent/tests/run_events_contract.rs`, `crates/vv-agent/tests/run_event_validation.rs`, `crates/vv-agent/tests/configured_sub_agent_parity.rs`, `crates/vv-agent/tests/runner_checkpoint.rs` |
| Run budgets | `crates/vv-agent/src/budget.rs`, `crates/vv-agent/src/runtime/engine/budget.rs`, `crates/vv-agent/tests/run_budget.rs` |
| After-cycle lifecycle hooks | `crates/vv-agent/src/runtime/lifecycle.rs`, `crates/vv-agent/src/runtime/engine/lifecycle.rs`, `crates/vv-agent/src/runtime/run_definition.rs`, `crates/vv-agent/src/runtime/backends/distributed/`, `crates/vv-agent/tests/runtime_cycle/after_cycle.rs`, `crates/vv-agent/tests/distributed_checkpoint.rs` |
| Completion policy and terminal observations | `crates/vv-agent/src/runner/`, `crates/vv-agent/src/runtime/engine/`, `crates/vv-agent/tests/completion_policy_contract.rs`, `crates/vv-agent/tests/approval_resume_completion.rs`, `crates/vv-agent/tests/runner_terminal_contract.rs` |
| Tool metadata in checkpoint and durable execution | `crates/vv-agent/src/runtime/run_definition.rs`, `crates/vv-agent/src/checkpoint/canonical.rs`, `crates/vv-agent/src/runtime/checkpoint_resume.rs`, `crates/vv-agent/src/runtime/checkpoint_resume/persistence.rs`, `crates/vv-agent/src/runtime/backends/distributed/`; `crates/vv-agent/tests/checkpoint_core.rs`, `crates/vv-agent/tests/runner_checkpoint.rs`, `crates/vv-agent/tests/distributed_checkpoint.rs` |
| Distributed runtime | `crates/vv-agent/src/runtime/backends/distributed/`, `crates/vv-agent/src/runtime/checkpoint_codec.rs`, `crates/vv-agent/tests/distributed_checkpoint.rs` |
| App Server tool lifecycle projection | `crates/vv-agent/src/app_server/protocol/item.rs`, `crates/vv-agent/src/app_server/run_adapter.rs`; `crates/vv-agent/tests/app_server_thread_turn.rs`, `crates/vv-agent/tests/app_server_contract_parity.rs` |
| Real closure tests | `crates/vv-agent/tests/parity_evidence_manifests.rs`, `crates/vv-agent/tests/runner_producer_parity.rs` |

A fixture parser or private helper test cannot replace a real public producer
test. A field that is declared but ignored by a planner, executor, provider, or
store remains a contract failure.

Distributed workers and dispatchers exchange only the closed
`vv-agent.distributed-worker-response.v1` wire. The implementation in
`runtime/backends/distributed/dispatch.rs` has exactly `pending`, `committed`,
`terminal_candidate`, and `terminal_replay` variants. The replaced `finished`
and terminal boolean combination is neither produced nor accepted. A candidate
accepts reconciliation-required or terminal/interrupted results; a replay
rejects reconciliation-required and must equal the retained durable result.
The scheduler reloads the authoritative checkpoint after every response or
transport failure. Public `AgentResult` readers require all 13 current fields,
reject unknown fields, and require absent optional budget/error fields to be
omitted rather than encoded as null.

## Memory Capacity Mapping

Rust records a resolved model's output capability in task metadata as
`model_max_output_tokens`. It does not synthesize `reserved_output_tokens` from
that capability. This projection is preserved through the main Runner path,
checkpoint reconstruction, and configured sub-agent creation.

`runtime/engine/memory.rs` resolves the context window from positive explicit
task metadata and resolved model capability. When neither is known, it derives
the planning context from the configured positive compaction threshold (or
`250000`) plus the selected output reserve and the `13000` auto-compaction
buffer; the default is therefore `279000`. It resolves output reserve from an effective positive
`ModelSettings.max_tokens`, explicit task metadata, then the `16000` framework
fallback. Only that fallback may be capped by a smaller
`model_max_output_tokens` capability. The memory manager subtracts the `13000`
default auto-compaction buffer and preserves a known derived capacity of zero
from a positive context. Omitted task and manager compact thresholds default to
`250000`; explicit values in durable tasks remain unchanged.

The runtime microcompacts eligible old tool results before evaluating an
optional warning against recalculated usage, including when the original usage
also crossed the full-compaction threshold. It emits every new capacity field
on `memory_compact_started`, then emits the strongest applied mode and a
message-content comparison as `changed` on `memory_compact_completed`.
Provider callbacks, runtime payloads, and `runner/event_stream.rs` journal
projections reuse the same `event_id` and `created_at`. The current `v1`
decoder rejects missing, unknown, stale, and malformed fields; it has no
alternate historical decoder. No capacity or compaction branch inspects
task category, answer meaning, or semantic progress.

Runner checkpoint resume restores `run_metadata` from the frozen run definition
before rebuilding runtime controls; current caller metadata does not rewrite
the behavior-affecting snapshot.

## Output Validation Mapping

Rust registers a `host_output_validator` and optional `output_repair` callback
on `AgentBuilder`; `output_validation_enabled` remains false unless the host
opts in. The validator receives the Rust public final-output string and an
`OutputValidationContext` containing run identity, agent identity, and the
existing output type name. This maps to Python receiving its own public,
possibly coerced final-output value.

The existing Rust typed deserialization check runs before host validation. A
typed-output failure may enter the one permitted repair, and a replacement
must pass both deserialization and the same host validator. The canonical empty
repair-tool collection maps to an empty `Vec<Value>`; it does not create a
second registry or another model cycle.

Validation and repair run before session persistence, checkpoint finalization,
and terminal-event emission. Rejection sets
`RunResult::error_code() == Some("output_validation_failed")` and commits one
failed terminal. Successful repair commits one completed terminal with the
replacement. Terminal checkpoint replay reuses the validated result without
calling either host callback. Producer coverage lives in
`tests/output_validation_contract.rs`, `tests/runner_checkpoint.rs`, and
`tests/approval_resume_completion.rs`.

## Rust Adaptations

The following are API-shape adaptations, not behavioral differences:

- structs, traits, builders, generics, and `Result` map to Python dataclasses,
  protocols, decorators, and exceptions;
- async methods and blocking wrappers may coexist where Python exposes
  synchronous convenience APIs;
- typed deserialization maps to Python `output_type` coercion;
- Rust validates its string final-output representation and exposes the output
  type name in the callback context; Python validates its public, possibly
  coerced value and exposes the output type object. Both preserve the same
  typed-output gate, at-most-once repair, terminal, and replay behavior.
- Apalis adapters map to Python Celery adapters through the same distributed
  envelope, checkpoint, lease, and terminal-state contract;
- Rust `ModelProvider` controls map to Python settings-file and provider
  capabilities.
- Rust names the coarse enum `ToolSideEffect` and attaches `ToolMetadata`
  through builders and trait accessors. These are language-idiomatic API shapes
  for the same closed declaration, normalization, policy, event, and durable
  behavior.
- The exported Rust `ToolLifecycleCallback` and `ToolLifecycleEvent` are a
  low-level language-side observation adapter that feeds the shared
  planned/started/completed lifecycle into runtime events. They do not add a
  central contract event, decision, delivery guarantee, or terminal semantic.
- Rust exposes immutable snapshot structs and a trait object; Python uses
  copied frozen dataclasses and a protocol callback. Both compose
  runner-default hooks before per-run hooks, persist only cumulative denials,
  and resolve distributed `after_cycle_hook_refs` before checkpoint claim.

Add a new adaptation only when both implementations preserve input, output,
safety, persistence, cancellation, and lifecycle semantics.

## Completion Gate

```bash
python3 scripts/contract_snapshot.py check --source ../vv-agent-contract
cargo fmt --all -- --check
cargo test -p vv-agent -- --test-threads=1
cargo check --examples
cargo clippy --all-targets --all-features -- -D warnings
```

Then run the Python gate and the central
`vv-agent-contract/.github/workflows/cross-repository.yml` workflow with exact
contract, Python, and Rust refs. If either implementation is incomplete, keep
the central support matrix at `pending-adoption` or `in-progress`.
