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

The current lock selects contract `8.0.1` at revision
`24c5b365f03d211d552df998c1f30a0f7cbf4f0f`, release artifact SHA-256
`8bbf4eca2a57c1c5e99c7f11f3dda8c7000a4af7125e633458106c2fa0c08a53`.
The current adoption state is not duplicated in this document. Treat
[`vv-agent-contract/support-matrix.json`](https://github.com/AndersonBY/vv-agent-contract/blob/main/support-matrix.json)
as the machine-readable source for the current verified Python and Rust
revisions, verification timestamp, and cross-repository run URL.

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
| Resolved PromptBundle and one-run producer scope | `crates/vv-agent/src/agent.rs`, `crates/vv-agent/src/runner.rs`, `crates/vv-agent/src/runner/run_single.rs`, `crates/vv-agent/src/runtime/engine/model_request.rs`, `crates/vv-agent/src/llm/`; `crates/vv-agent/tests/context_providers.rs`, `crates/vv-agent/tests/runner_checkpoint.rs`, `crates/vv-agent/tests/parity_evidence_manifests.rs` |
| Built-in tool specification | `crates/vv-agent/src/tools/`, `crates/vv-agent/tests/tool_schema_contract.rs` |
| Canonical 15-tool surface and removed model memory tool | `crates/vv-agent/src/tools/registry/defaults.rs`, `crates/vv-agent/src/constants/tool_names.rs`, `crates/vv-agent/src/tools/executor.rs`; `crates/vv-agent/tests/parity_evidence_manifests.rs`, `crates/vv-agent/tests/tool_schema_contract.rs`, `crates/vv-agent/tests/builtin_tool_behavior_contract.rs` |
| Sparse bounded tool results, artifact recovery, and read cursor | `crates/vv-agent/src/types/tool_calls.rs`, `crates/vv-agent/src/types/dict/tools.rs`, `crates/vv-agent/src/workspace/artifacts.rs`, `crates/vv-agent/src/tools/handlers/bash/execution.rs`, `crates/vv-agent/src/tools/handlers/background.rs`, `crates/vv-agent/src/tools/handlers/workspace/file_io/read.rs`; `crates/vv-agent/tests/bounded_tool_result_contract.rs`, `crates/vv-agent/tests/bash_tools.rs`, `crates/vv-agent/tests/workspace_tools.rs` |
| Typed tool declaration and public propagation | `crates/vv-agent/src/tools/metadata.rs`, `crates/vv-agent/src/tools/function.rs`, `crates/vv-agent/src/tools/public_tool.rs`, `crates/vv-agent/src/tools/base/spec.rs`, `crates/vv-agent/src/tools/executor.rs`, `crates/vv-agent/src/tools/registry/mod.rs`; `crates/vv-agent/tests/tool_metadata_contract.rs`, `crates/vv-agent/tests/parity_evidence_manifests.rs`, `crates/vv-agent/tests/tool_orchestrator.rs`, `crates/vv-agent/tests/tool_schema_contract.rs` |
| Metadata denial policy and delegation | `crates/vv-agent/src/tools/policy.rs`, `crates/vv-agent/src/runner/support.rs`, `crates/vv-agent/src/runtime/tool_planner.rs`, `crates/vv-agent/src/runtime/sub_agents/`, `crates/vv-agent/src/runner/handoff.rs`, `crates/vv-agent/src/runtime/backends/distributed/`; `crates/vv-agent/tests/runner_tool_policy.rs`, `crates/vv-agent/tests/configured_sub_agent_parity.rs`, `crates/vv-agent/tests/agent_tool_contract.rs`, `crates/vv-agent/tests/handoff_contract.rs`, `crates/vv-agent/tests/distributed_checkpoint.rs` |
| Agent, Runner, result, live control | `crates/vv-agent/src/agent.rs`, `crates/vv-agent/src/runner/`, `crates/vv-agent/src/run_handle.rs` |
| Optional output validation and repair | `crates/vv-agent/src/output_validation.rs`, `crates/vv-agent/src/agent.rs`, `crates/vv-agent/src/runner/`, `crates/vv-agent/tests/output_validation_contract.rs` |
| Delegation and background tasks | `crates/vv-agent/src/tools/background_agent_task.rs`, `crates/vv-agent/src/handoffs.rs`, `crates/vv-agent/src/runtime/sub_agents/` |
| Sessions and stores | `crates/vv-agent/src/sessions.rs`, `crates/vv-agent/src/runtime/stores/`, `crates/vv-agent/tests/session_store_parity.rs` |
| Events and tracing | `crates/vv-agent/src/events/`, `crates/vv-agent/src/event_store.rs`, `crates/vv-agent/src/runtime/model_calls.rs`, `crates/vv-agent/src/tracing.rs`; `crates/vv-agent/tests/run_events_contract.rs`, `crates/vv-agent/tests/run_event_validation.rs` |
| Tool planned/started/completed lifecycle | `crates/vv-agent/src/tools/orchestrator.rs`, `crates/vv-agent/src/runtime/engine/tool_batch.rs`, `crates/vv-agent/src/events.rs`, `crates/vv-agent/src/events/wire.rs`, `crates/vv-agent/src/runner/event_stream.rs`, `crates/vv-agent/src/runner/resume.rs`; `crates/vv-agent/tests/runtime_cycle/hooks.rs`, `crates/vv-agent/tests/runner_producer_parity.rs`, `crates/vv-agent/tests/run_events_contract.rs`, `crates/vv-agent/tests/run_event_validation.rs`, `crates/vv-agent/tests/approval_resume_completion.rs` |
| Model stream projection | `crates/vv-agent/src/events/`, `crates/vv-agent/src/runner/event_stream/stream_projection.rs`, `crates/vv-agent/src/runner/run_single.rs`, `crates/vv-agent/src/runtime/sub_agents/events.rs`, `crates/vv-agent/src/app_server/protocol/item.rs`, `crates/vv-agent/tests/runner_producer_parity.rs` |
| Model-call ledger, token, and cache usage | `crates/vv-agent/src/types/token_usage.rs`, `crates/vv-agent/src/runtime/model_calls.rs`, `crates/vv-agent/src/runtime/token_usage.rs`, `crates/vv-agent/src/runtime/checkpoint_resume/operations.rs`, `crates/vv-agent/src/llm/vv_llm_client/`; `crates/vv-agent/tests/token_usage.rs`, `crates/vv-agent/tests/runtime_cycle/session_memory.rs`, `crates/vv-agent/tests/runner_checkpoint.rs` |
| Assistant reasoning history | `crates/vv-agent/src/memory/message_sanitizer.rs`, `crates/vv-agent/src/llm/vv_llm_client/`, `crates/vv-agent/tests/message_sanitizer.rs`, `crates/vv-agent/tests/completion_policy_contract.rs` |
| Memory capacity, Session Memory, and compaction lifecycle | `crates/vv-agent/src/config.rs`, `crates/vv-agent/src/memory/`, `crates/vv-agent/src/runtime/engine/memory/`, `crates/vv-agent/src/runner/event_stream.rs`, `crates/vv-agent/src/events/`; `crates/vv-agent/tests/memory_lifecycle_contract.rs`, `crates/vv-agent/tests/runtime_cycle/microcompact.rs`, `crates/vv-agent/tests/runtime_cycle/session_memory.rs`, `crates/vv-agent/tests/run_events_contract.rs`, `crates/vv-agent/tests/configured_sub_agent_parity.rs`, `crates/vv-agent/tests/runner_checkpoint.rs` |
| Run budgets | `crates/vv-agent/src/budget.rs`, `crates/vv-agent/src/runtime/engine/budget.rs`, `crates/vv-agent/tests/run_budget.rs` |
| After-cycle lifecycle hooks | `crates/vv-agent/src/runtime/lifecycle.rs`, `crates/vv-agent/src/runtime/engine/lifecycle.rs`, `crates/vv-agent/src/runtime/run_definition.rs`, `crates/vv-agent/src/runtime/backends/distributed/`, `crates/vv-agent/tests/runtime_cycle/after_cycle.rs`, `crates/vv-agent/tests/distributed_checkpoint.rs` |
| Completion policy and terminal observations | `crates/vv-agent/src/runner/`, `crates/vv-agent/src/runtime/engine/`, `crates/vv-agent/tests/completion_policy_contract.rs`, `crates/vv-agent/tests/approval_resume_completion.rs`, `crates/vv-agent/tests/runner_terminal_contract.rs` |
| Tool metadata in checkpoint and durable execution | `crates/vv-agent/src/runtime/run_definition.rs`, `crates/vv-agent/src/checkpoint/canonical.rs`, `crates/vv-agent/src/runtime/checkpoint_resume.rs`, `crates/vv-agent/src/runtime/checkpoint_resume/persistence.rs`, `crates/vv-agent/src/runtime/backends/distributed/`; `crates/vv-agent/tests/checkpoint_core.rs`, `crates/vv-agent/tests/runner_checkpoint.rs`, `crates/vv-agent/tests/distributed_checkpoint.rs` |
| Distributed runtime | `crates/vv-agent/src/runtime/backends/distributed/`, `crates/vv-agent/src/runtime/checkpoint_codec.rs`, `crates/vv-agent/tests/distributed_checkpoint.rs` |
| App Server lifecycle and usage projection | `crates/vv-agent/src/app_server/protocol/`, `crates/vv-agent/src/app_server/run_adapter.rs`; `crates/vv-agent/tests/app_server_thread_turn.rs`, `crates/vv-agent/tests/app_server_contract_parity.rs` |
| Real closure tests | `crates/vv-agent/tests/parity_evidence_manifests.rs`, `crates/vv-agent/tests/runner_producer_parity.rs` |

A fixture parser or private helper test cannot replace a real public producer
test. A field that is declared but ignored by a planner, executor, provider, or
store remains a contract failure.

## Contract 8.0.1 Boundaries

### Prompt Bundle And Provider Projection

`PromptBundle` is the only resolved system-prompt representation after a run
starts. `AgentTask`, `LlmRequest`, run definitions, checkpoints, and
distributed envelopes carry it explicitly. Generic metadata does not carry
prompt sections, sources, or stable hashes. Non-section-aware providers receive
one flattened system message; Anthropic projection may retain section
boundaries only to place canonical cache breakpoints.

Instruction providers, context providers, and the run clock execute once while
compiling a new run. All model cycles reuse that immutable bundle. Checkpoint
resume and terminal replay restore `prompt_bundle` from the frozen run
definition without calling those producers or reading the clock again. A
separately started run compiles its own volatile sections while the stable hash
continues to cover only stable section objects.

### Bounded Tool Results And Tool Surface

Ordinary `ToolExecutionResult` values serialize only required fields and
present optional fields. A truncated result additionally carries its reason,
byte counts, and either an artifact or cursor. Bash keeps a deterministic
12,000-character head/tail preview and writes complete output below
`.vv-agent/artifacts/`; background polling reuses the same terminal artifact.
`read_file` returns bounded text plus a `read_file` cursor containing the
normalized path, source SHA-256, and Unicode-scalar offset. Cursor recovery
rejects path mismatches, changed content, and out-of-range offsets.

For a local workspace, `.vv-agent/artifacts/` is a logical recovery namespace
mapped to private storage outside the shell working directory. Complete
truncated terminal output is streamed into one exclusive immutable artifact;
the runtime does not materialize the full capture in application memory, and
shell commands cannot mutate recovery bytes. Recovery still passes through the
normal workspace and `read_file` policy boundary.

The current built-in manifest is `vv-agent-builtin-tools-v2` with 15
model-visible tools. Its fixture schema version is `2`; the canonical
distributed `ToolsetRef.version` remains `1`. `ToolExposure` contains only
`direct` and `hidden`. The model-visible `compress_memory` tool,
`memory_notes` state, and `deferred` exposure do not exist; framework-owned
automatic compaction remains internal.

### Model Calls And Events

Every primary Agent cycle, Session Memory extraction, and full memory
compaction request passes through one `ModelCallCoordinator`. Each actual
dispatch emits `model_call_started` and exactly one terminal
`model_call_completed` or `model_call_failed` event. The event and ledger
record share call id, operation id, attempt, operation, cycle, backend, model,
usage, and error outcome.

Task-neutral observations remain typed diagnostics. A diagnostic cannot replace
model, budget, cancellation, tool, approval, checkpoint, or terminal lifecycle
events. `RunEvent` version `v4` is the strict current wire discriminator; stale,
missing, unknown, and malformed fields are rejected rather than routed through
an older decoder.

### Durable Accounting

Checkpoints require `vv-agent.checkpoint.v8`, and run definitions require
`vv-agent.run-definition.v5`. The run definition stores `prompt_bundle` and
never stores an independent flattened prompt. The checkpoint owns the complete
ordered run-level model-call ledger. A started model journal entry and started
event become durable together. After dispatch, the terminal journal state,
ledger record, budget observation, provider response receipt, and terminal event
become durable together and must agree on identity.

Receipt replay returns the stored model response without another provider
dispatch, ledger append, or budget increment. Session Memory then reapplies its
derived merge from that response. The merge key is the normalized category and
case-folded, whitespace-normalized content, so replay does not duplicate an
existing fact. Producer coverage for the crash boundary and terminal replay is
in `crates/vv-agent/tests/runner_checkpoint.rs`.

### Model Usage And Memory

`TaskTokenUsage v2` contains the ordered `model_calls` ledger. Aggregate token
and cache values are derived from that ledger; an empty ledger has exact zero
totals, while a missing measurement in any dispatched attempt keeps the
corresponding aggregate unavailable rather than inventing zero. `CycleRecord`
does not duplicate usage.

Session Memory defaults to disabled. Only the exact boolean
`session_memory_enabled=true` enables its prompt injection, storage access,
workspace writes, or model dispatch. Existing files, seed data, parent
configuration, and the removed `enable_session_memory` alias do not activate
it. Internal memory calls use the configured provider route or the primary
client only when that route is the default; explicit backend selection never
silently reuses an unrelated direct client. Cancellation, budget exhaustion,
checkpoint interruption, and checkpoint integrity errors propagate through the
runtime control path instead of being swallowed by memory fail-soft behavior.

When enabled, a newly compiled run reads persisted entries once and freezes
them into its `PromptBundle`. Extraction during that run may persist new entries
but never rewrites the active bundle; those entries become model-visible only
in a later newly compiled run. Checkpoint resume restores the frozen section
without rereading the store.

### App Server

Model-call lifecycle events project to `modelCall` items with the same seven
identity fields and terminal accounting. Terminal `tokenUsage` recursively
camel-cases the complete task usage object, including `modelCalls` and
`cacheUsage`, while opaque provider-native keys inside `providerUsage` remain
unchanged.

Distributed workers accept only `vv-agent.distributed-run.v5`; its `task`
contains `prompt_bundle` and has no `system_prompt` field. Workers and
dispatchers exchange only the closed
`vv-agent.distributed-worker-response.v3` wire. The implementation in
`runtime/backends/distributed/dispatch.rs` has exactly `pending`, `committed`,
`terminal_candidate`, and `terminal_replay` variants. The replaced `finished`
and terminal boolean combination is neither produced nor accepted. A candidate
accepts reconciliation-required or terminal/interrupted results; a replay
rejects reconciliation-required and must equal the retained durable result.
The scheduler reloads the authoritative checkpoint after every response or
transport failure. Public `AgentResult` readers require all 13 current fields,
reject unknown fields, and require absent optional budget/error fields to be
omitted rather than encoded as null.

The transport-neutral nonblocking scheduler API is implemented in
`runtime/backends/distributed/driver.rs`. `start` enqueues at most the first
cycle and returns a passive `DistributedRunHandle`; `advance` performs exactly
one authoritative checkpoint read and returns `Dispatch`, `RetryAt`, `Wait`,
`FinalizeRequired`, or `TerminalReplay`. Superseded callbacks are no-op waits.
The Apalis adapter is enqueue-only and uses `TaskSink` without
`WaitForCompletion`; no result-polling dispatcher is part of the public
surface. A terminal candidate, including synthetic max-cycles exhaustion, must be passed to a
separate framework terminal controller. `Runner::start_distributed` prepares
the durable checkpoint and returns the passive handle;
`Runner::finalize_distributed` consumes only `FinalizeRequired` and reuses the
normal guardrail, validation, append-once session, outbox, claim-bound or
revision-bound CAS, delivery, and acknowledgement path. Duplicate finalizer
delivery returns the retained terminal. Durable cross-process approval
continuation is not implemented by this Rust slice.

## Durable Deferred Tools

Contract 8 adds one provider-neutral result boundary for tools whose external
acceptance finishes after the current worker invocation. The framework creates
an opaque `DeferredToolHandle` through `ToolContext::defer`; handlers never
construct checkpoint journals, claims, provider/job identifiers, or callback
metadata. Without an active durable checkpoint the factory returns a completed
`ERROR` result with `deferred_requires_checkpoint` before an external effect.

`ToolCallOutcome` is the closed `vv-agent.tool-call-outcome.v2` wire: a
completed `ToolExecutionResult` or a deferred handle. Deferred is not a
`ToolExecutionResult` status. A model-tool batch is admitted once through the
checkpoint store (`admit_deferred_batch`), atomically persisting all completed
and deferred journal entries, lifecycle outbox events, the deferred barrier,
and one claim release. `CheckpointStatus::Deferred` blocks new model cycles
until every handle resolves.

`CheckpointStore::resolve_deferred(handle, result)` accepts only definitive
`SUCCESS` or `ERROR` results. Memory, SQLite, and Redis stores use an
independent receipt index and atomic resolution update; exact callbacks replay
the retained receipt, conflicting results return
`deferred_resolution_conflict`, invalid result statuses return
`deferred_resolution_result_invalid`, early callbacks return retryable
`deferred_resolution_not_admitted`, ambiguous operations require
reconciliation, stale identities return `deferred_resolution_stale`, and an
exact active handle on a claimed checkpoint returns
`deferred_checkpoint_claimed`. These are typed errors, not
`DeferredResolveDecision` variants.
Receipt cleanup follows checkpoint retention and has no fixed cardinality cap.

Recovery acceptance is an all-or-none `accept_deferred_batch` CAS under an
active recovery claim. It validates exact handles, records a
`reconciliation_resolved` audit plus `tool_call_deferred` events, and is
idempotent on exact replay without a second claim or revision. Distributed
workers keep the existing `vv-agent.distributed-worker-response.v3` pending
wire; the nonblocking driver waits with `deferred_pending` and performs no
worker polling or new response variant. App Server maps the state to a
non-terminal interrupted turn with `waitReason=deferred_pending` and a normal
`turn/resume` path.

The main Rust producer and evidence surfaces are
`crates/vv-agent/src/checkpoint/deferred.rs`,
`crates/vv-agent/src/runtime/state/deferred.rs`,
`crates/vv-agent/src/runtime/stores/{memory,sqlite,redis}.rs`,
`crates/vv-agent/src/tools/base/context.rs`,
`crates/vv-agent/src/runtime/backends/distributed/`, and
`crates/vv-agent/tests/deferred_tools.rs`.

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

The runtime resolves the public `MicrocompactionPolicy` into `AgentTask` with
defaults `0.75` trigger, `0.60` target, three recent cycles, and 500 minimum
characters. Checkpointed runs freeze and restore it under
`runtime_controls.microcompaction_policy`; it is not behavior-affecting
process-local metadata and does not add a capability ref.

The runtime plans eligible old `result_retention=archive` tool results oldest
first and applies that single plan once per cycle before evaluating an optional
warning against recalculated usage. Built-in and custom tools both default to
archive; `preserve` excludes only proactive microcompaction. Complete text is
written through the effective workspace backend to `.vv-agent/artifacts/`
before replacement. An existing typed `Message.artifact_ref` is reused only
after its complete UTF-8 bytes pass `size_bytes` and SHA-256 validation.
Missing or corrupt references keep the original message, and recovery
envelopes without a typed reference are never archived again. New artifacts
are created only for ordinary complete results. The replacement retains the
complete typed reference through host, session, checkpoint, and distributed
round trips; LLM/model projection omits it. Persistence failure keeps the
original message while the same application pass continues to later
candidates. The compact marker exposes only `tool_name`, `artifact_path`, the
fixed `use read_file` retrieval hint, and an excerpt.
When `read_file` is absent from the task's actual model-visible tool plan,
proactive microcompaction and full-compaction pre-archiving do not create that
unusable marker.

Application stops at the target using each actual replacement token
difference rather than the plan estimate. The same operation is public as
`MemoryManager::microcompact_messages`.

The model-visible replacement has this closed shape:

```text
<Tool Result Compact>
tool_name: web_search
artifact_path: .vv-agent/artifacts/<run>/<call>.txt
retrieval_hint: use read_file on artifact_path if needed
excerpt:
<bounded head/tail preview>
</Tool Result Compact>
```

Artifact byte size and SHA-256 remain host-only integrity fields and never
appear in the marker. SQLite session persistence uses the strict current schema
at `PRAGMA user_version=2`.

A micro-threshold crossing without a candidate emits no compaction lifecycle
event. `memory_compact_started` includes `microcompact_target`,
`candidate_count`, and `estimated_reclaimable_tokens`;
`memory_compact_completed` includes `archived_count`, `reclaimed_tokens`, and
`artifact_failure_count`, plus the strongest applied mode and a
message-content comparison as `changed`.
Provider callbacks, runtime payloads, and `runner/event_stream.rs` journal
projections reuse the same `event_id` and `created_at`. The current `v4`
decoder rejects missing, unknown, stale, and malformed fields; it has no
alternate historical decoder. No capacity or compaction branch inspects
task category, answer meaning, or semantic progress.

Runner checkpoint resume restores `run_metadata` and typed runtime controls
from the frozen run definition; current caller metadata or
`RunConfig.microcompaction_policy` does not rewrite the snapshot.

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
- Both event stores use the typed `RunEventReplayQuery` for lineage replay and
  include direct children by default. Python additionally offers a `run_id=`
  convenience keyword; Rust keeps the root method as
  `replay(RunEventReplayQuery)` so `include_children` remains explicit. Rust's
  `RunHandle::events()` is the live typed-event projection; durable replay is
  intentionally a separate `RunEventStore` operation.

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

The current Rust gate may emit ts-rs warnings that it cannot parse the serde
attributes `deny_unknown_fields` and `deserialize_with =
"deserialize_input_items"`. These warnings are from TypeScript metadata
generation; the runtime serde readers still enforce the strict v8 wire. The
attributes must remain on the Rust readers until ts-rs supports them rather
than being removed to silence the warning.
