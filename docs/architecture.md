# Architecture

`vv-agent-rs` is a Rust workspace for the `vv-agent` crate. The crate provides
an agent runtime, SDK, CLI, built-in tools, memory management, execution
backends, and workspace backends. Provider protocol details are intentionally
delegated to the `vv-llm` crate.

## Top-Level Flow

```text
CLI / SDK / embedding application
  -> Agent + Runner
      -> ModelProvider / ModelRef / ModelSettings
      -> RunConfig / Session / Tool APIs / Providers
      -> Runner::run / Runner::start / Runner::stream
      -> RunHandle / RunEventStream / RunResult
      -> compile to runtime task
  -> RunEventStore / TraceSink
  -> config::load_llm_settings_from_file
  -> config::resolve_model_endpoint
  -> llm::VvLlmClient
  -> runtime::AgentRuntime
      -> cycle runner
      -> memory manager
      -> tool planner
      -> tool orchestrator
      -> execution backend
  -> RunResult / AgentResult
```

The backward-compatible default is tool-driven: `task_finish` finishes and
`ask_user` waits for input. `Agent` and `RunConfig` can explicitly select a
no-tool finish or wait policy. The runtime applies that control without
classifying assistant text or inferring task-specific success.

Raw model stream callbacks are synchronous at-least-once observers, not a
durable event store. The Runner projects only assistant/reasoning deltas and
model tool-call start/progress into typed events with framework-owned identity
and cycle fields. Model tool generation uses `model_tool_call_*`; actual tool
execution continues to use `tool_call_started` / `tool_call_completed`.
Reasoning remains private telemetry and is not rendered as App Server answer
text. Unknown or malformed raw stream payloads stay raw-only.

Token accounting keeps provider truth separate from compatibility values.
`TokenUsage::usage_source` identifies provider-reported, estimated, or missing
totals. `CacheUsage` distinguishes an explicit zero cache read from missing
accounting and adapter-declared lack of support. `TaskTokenUsage` exposes a
cache total only when every included cycle reports that metric; legacy numeric
fields remain available but do not prove cache-accounting availability.

## Module Map

| Path | Responsibility |
| --- | --- |
| `crates/vv-agent/src/config/` | Settings literal parsing, API key decoding, backend normalization, endpoint lookup, and exact model resolution. |
| `crates/vv-agent/src/cli/` | CLI argument parsing, task construction, runtime logging, and output payloads. |
| `crates/vv-agent/src/agent.rs` | Public `Agent` builder for instructions, model defaults, tools, handoffs, hooks, and metadata. |
| `crates/vv-agent/src/runner.rs` | Public `Runner` that resolves models, compiles public inputs, starts live handles, streams typed events, and reuses the runtime engine. |
| `crates/vv-agent/src/run_handle.rs` | Live run handle for event subscription, result synchronization, cancellation, state reads, and approval decisions. |
| `crates/vv-agent/src/interactive.rs` | Embedded stateful session facade for stable identity, live steering, follow-ups, cancellation, and session events. |
| `crates/vv-agent/src/run_config.rs` | Per-run overrides for model, workspace, bounds, tool registry/policy, runtime injection, diagnostics, providers, session, hooks, cancellation, event store, and metadata. |
| `crates/vv-agent/src/budget.rs` | Task-neutral run limits, cumulative observations, host-cost meter protocol, and deterministic enforcement. |
| `crates/vv-agent/src/approval.rs` | Host approval protocol, broker, request payload, async decision future, and approval errors. |
| `crates/vv-agent/src/context_providers.rs` | Context fragment collection, ordering, prompt-budget assembly, source metadata, and stable fragment reporting. |
| `crates/vv-agent/src/event_store.rs` | Append-only run event storage and replay query contract, including JSONL storage. |
| `crates/vv-agent/src/model.rs` | Public `ModelRef`, `ModelProvider`, `VvLlmModelProvider`, and scripted provider contracts. |
| `crates/vv-agent/src/model_settings.rs` | Public model-call settings aligned with common `vv-llm` request options. |
| `crates/vv-agent/src/sessions.rs` | Public `Session` storage contract and in-memory implementation. |
| `crates/vv-agent/src/events.rs` | v1 run event envelope and typed serializable payloads for SDK consumers. |
| `crates/vv-agent/src/types/` | Public protocol types, dictionaries, messages, tasks, statuses, records, and token usage. |
| `crates/vv-agent/src/llm/` | LLM trait, scripted test client, `vv-llm` bridge, endpoint failover, streaming, prompt cache, and request normalization. |
| `crates/vv-agent/src/runtime/` | Agent runtime, cycle execution, hooks, cancellation, shell runtime, background sessions, sub-agents, state stores, and execution backends. |
| `crates/vv-agent/src/tools/` | Tool registry, public `Tool`/`FunctionTool` APIs, executor/orchestrator contracts, schemas, dispatcher, shared parsing helpers, and built-in handlers. |
| `crates/vv-agent/src/constants/` | Stable tool names and model-visible schema constants. |
| `crates/vv-agent/src/memory/` | Token counting, compaction, external memory provider hooks, artifact storage, session memory, micro-compaction, prompt-too-long handling, and file-context restoration. |
| `crates/vv-agent/src/prompt/` | System prompt sections, prompt-cache break tracking, available skills, and prompt hashes. |
| `crates/vv-agent/src/agent.rs`, `runner.rs`, `run_config.rs`, `sessions.rs` | Public `Agent` + `Runner`, run configuration, and session storage. |
| `crates/vv-agent/src/workspace/` | Local, memory, and S3-compatible workspace backends. |
| `crates/vv-agent/src/skills/` | Skill discovery, frontmatter parsing, normalization, validation, prompt rendering, and activation state. |

## Execution Backends

- Inline backend: synchronous default execution.
- Thread backend: non-blocking execution with task submission.
- Distributed backend: checkpointed cycle execution with inline fallback and pluggable dispatchers.
- Checkpoint stores: in-memory, SQLite, and Redis.

Distributed and checkpointed paths must preserve the same public result and
checkpoint payload shape as inline execution.

Optional run budgets are evaluated at stable runtime boundaries shared by all
backends. Inline and thread runs keep one evaluator for the active run.
Distributed limits travel in each envelope, while cumulative usage is stored
in the checkpoint so each worker adds only its active monotonic segment. Host
cost remains a worker-local capability and is never reconstructed from a price
table. See `run-budgets.md` for public API and terminal precedence.

Distributed mode sends a versioned `DistributedRunEnvelope` for each cycle.
Workers resolve all referenced capabilities before claiming state, then use a
revision/token lease with heartbeat renewal and CAS commit. The scheduler
accepts a result only after reconciling it with the durable checkpoint;
terminal checkpoints are immutable and replayable until acknowledged. SQLite
uses WAL, a bounded busy timeout, and in-place legacy-column migration.
Before entering the runtime cycle, a worker must complete one successful lease
renewal; initial and renewed lease expiry never extends beyond the job deadline.
Each periodic wait is derived from that renewal's actual deadline-clamped lease,
not only from the configured duration. A renewal result must return before both
the previously known expiry and the new expiry it requested. Response checks
use the conservative maximum of current wall time and request-start wall time
plus monotonic elapsed time, covering wall-clock jumps in either direction.
SQLite refreshes effective time after acquiring its write lock. Redis renewal
uses one atomic script: Redis `TIME` validates both expiries and the original
JSON is the compare-and-set value, so an expired or replaced owner cannot write
a new expiry. The script distinguishes CAS loss from authoritative expiry, so
commit-race suppression can apply only to claim consumption and never to an
expired lease; authoritative expiry takes precedence when both conditions are
observed. Heartbeat renewal uses an independent store connection and remains
active through an explicit commit phase. A durable commit suppresses only an
active-claim rejection from a renewal that started in that commit phase and
returned before its applicable lease expiry. Renewals that started before
commit, expired leases, and other coordination failures remain visible even if
the checkpoint commit later succeeds. Rust's public `run_checkpointed_cycle`
helper uses this same lifecycle with the default lease and no job deadline.
Redis connection I/O and non-renewal optimistic-transaction retries are bounded
so stopping or unwinding a worker cannot wait forever on the heartbeat thread.

This is an at-least-once execution model. Apalis cancellation stops scheduler
polling but queued or claimed work may still complete. The cycle idempotency
key does not provide an event outbox, durable cancellation record, or
idempotency for external tool side effects. See `parity-contract.md` for the
complete cross-language contract.

## Public SDK

The public path is `Agent` + `Runner`. Public types express user intent;
the runner compiles those values into the existing runtime payload and keeps
`AgentRuntime`, `CycleRunner`, `ToolOrchestrator`, and backend code as the
execution layer.

Core responsibilities:

- `Agent`: name, instructions, model default, model settings, public tools,
  `Agent::as_tool()`, handoffs, hooks, and metadata.
- `Runner`: model provider, workspace default, default tool registry, run and
  live entrypoints.
- `RunConfig`: per-call model, model settings, workspace, bounds, session,
  tool registry/policy, runtime message injection and observers, diagnostics,
  hooks, after-cycle lifecycle hooks, cancellation, optional run budgets,
  public `ExecutionMode`, providers, event store, and metadata override. See
  `runtime-control.md` for the complete surface.
- `RunHandle`: live event stream, cancellation, state, approval decisions, and
  final result synchronization.
- `InteractiveAgentClient` / `InteractiveSession`: embedded stateful control over
  `Runner`, `RunHandle`, and `Session`, including steering and queued follow-up
  turns.
- `RunEvent`: v1 envelope with stable identity fields and a typed payload.
- `RunEventStore`: append-only event storage and replay by run lineage. Replay
  includes direct child runs by default; callers can explicitly request only
  the selected run.
- `ApprovalProvider`: host-driven live tool approval. `ask_user` remains the
  model-facing tool for asking a user to provide conversational input.
- `ToolPolicy` approval modes are `Default`, `Always`, `Never`, and
  `OnRequest`. `Default` is the unset merge sentinel; explicit `OnRequest`
  follows tool declarations, while `Always` and `Never` bypass dynamic tool
  approval predicates.
- `ContextProvider`: source-tracked, budgeted context fragments assembled into
  agent instructions before the run starts.
- `MemoryProvider`: external search/save and compaction lifecycle callbacks;
  `MemorySearchRequest.limit` defaults to `10`.
- `ModelProvider`: exact model resolution plus LLM client construction. The
  built-in `VvLlmModelProvider` uses repository settings through `vv-llm`, and
  `ScriptedModelProvider` is for unit tests.
- `FunctionTool`: typed argument parsing with structured `ToolOutput`, adapted
  into the registry-backed executor path; optional `ToolMetadata` stays on the
  host-visible spec/executor path and out of the model schema.
- `AgentTool`: public agent-as-tool wrapper that maps tool arguments into the
  existing `SubTaskRequest` runtime path.
- `Session`: history-only storage for `RunConfig`; background-task handles and
  interrupted-result resume are exposed through public APIs.

## Runtime Contracts

The runtime contract separates product concerns from framework concerns:

- Applications own UI timelines, approval dialogs, profile settings, product
  memory stores, notification channels, and durable production storage.
- The crate owns run lifecycle, event shape, tool execution policy, approval
  request flow, context assembly, memory provider callbacks, session graph
  lineage, and JSONL event replay.
- Applications can implement `ApprovalProvider`, `ContextProvider`,
  `MemoryProvider`, `RunEventStore`, `TraceSink`, and `ToolExecutor` without
  depending on internal runtime payloads.
- Applications can implement `AfterCycleHook` for an optional task-neutral
  control point after a complete cycle. It can steer the next cycle, add
  cumulative tool denials, or stop with failure; it cannot expand permissions
  or manufacture completed/waiting results.
- Product code should consume `RunEventPayload` for primary UI state instead of
  parsing raw runtime log strings.

## Tool Boundaries

Tool behavior is split so schemas and handlers can be tested independently:

- `tools/base/`: context, paths, and tool spec/result types.
- `tools/common/`: shared argument, path, grep, edit, and process helpers.
- `tools/handlers/`: concrete built-in tool behavior.
- `tools/executor.rs`: public executor adapter contract.
- `tools/orchestrator.rs`: per-call policy, approval, dispatch, timeout, and
  telemetry path.
- `tools/dispatcher.rs`: normalized dispatch and structured errors.
- `constants/tool_names.rs` and `constants/workspace.rs`: stable public names
  and model-visible schema constants.

### Typed Tool Declarations

`ToolMetadata` is an optional closed declaration carried separately from a
tool's generic `metadata` map:

| Rust field | Meaning |
| --- | --- |
| `side_effect: ToolSideEffect` | One of `Unknown`, `None`, `Read`, `Write`, `Execute`, `Network`, or `External`. Values have no hierarchy and are never inferred from names or arguments. |
| `idempotency: ToolIdempotency` | `Unknown`, `Supported`, or `Unsupported`; this participates in checkpoint recovery but does not make an external effect exactly once. |
| `terminal: bool` | The tool may return `finish` or `wait_user`; the declaration alone never changes run state. |
| `capability_tags: Vec<String>` | Opaque host labels matched as exact strings. |
| `cost_dimensions: Vec<String>` | Opaque resource names, not prices, measurements, units, or budget usage. |

An absent declaration stays `None`. For a present declaration,
`ToolMetadata::default()` is `Unknown` side effect, `Unknown` idempotency,
`terminal=false`, and two empty collections; wire input fills omitted fields
with the same defaults and rejects fields outside the closed set.

`FunctionTool::builder(...).tool_metadata(...)`,
`StaticTool::with_tool_metadata`, and the defaultable `Tool::tool_metadata` /
`ToolExecutor::tool_metadata` accessors are the public Rust propagation path.
`ToolSpec::tool_metadata` carries the same normalized declaration through the
registry. Generic keys named `side_effect`, `terminal`, or `capability_tags`
never become typed metadata.

The two label collections trim only tab, LF, CR, and ASCII space, reject blank
or longer-than-128-code-point labels, deduplicate exact matches, sort by UTF-16
code units, and reject more than 32 normalized entries. The existing
`FunctionTool::builder(...).idempotency(...)` input remains a compatibility
alias. A typed `Unknown` inherits a non-unknown legacy value; conflicting
non-unknown values fail construction with `tool_metadata_invalid`.

Typed metadata is host-visible only. `ToolRegistry::list_openai_schemas` still
projects `ToolSpec::schema`, so declarations do not alter function names,
descriptions, parameters, strictness, system prompts, or any other
model-visible bytes. When no declaration is present, the runtime does not
fabricate one from generic metadata.

### Denial-Only Tool Policy

`ToolPolicy` adds `denied_side_effects`, `denied_capability_tags`,
`deny_terminal_tools`, and `denied_cost_dimensions`, with public convenience
methods `deny_side_effect`, `deny_capability_tag`, `deny_terminal_tools`, and
`deny_cost_dimension`. Lists form a normalized set union across Agent,
Runner-default, and per-run policy; the boolean uses logical OR. Configured
sub-agents, agent-as-tool runs, handoffs, and distributed workers inherit the
effective parent denials and may only add more.

These checks are logically ANDed with existing allowed names, denied names,
argument predicates, planned names, approval, budgets, and runtime checks.
They cannot expose a tool or bypass another denial. The deterministic metadata
precedence is side effect, terminal, capability tag, then cost dimension; a
match returns `tool_not_allowed` with `policy_source` respectively set to
`metadata.side_effect`, `metadata.terminal`, `metadata.capability_tag`, or
`metadata.cost_dimension`. An absent typed declaration matches none of these
denials. A declared `ToolSideEffect::Unknown` can be denied explicitly.

### Executor Lifecycle

After serialized arguments normalize, one tool call has this typed lifecycle:

1. `tool_call_planned` before policy, approval, or dispatch;
2. zero or more approval events;
3. `tool_call_started` immediately before the executor may cause effects;
4. `tool_call_completed` after a `ToolExecutionResult` exists.

Invalid serialized arguments fail before planning. Unknown tools, policy
denials, and approval short-circuits produce planned plus completed without a
started event. Their completed event records `execution_started=false` and
`duration_ms=null`. Executed calls measure `duration_ms` from the started
boundary with a monotonic clock and also report lower-case `status`,
`directive`, and nullable `error_code`. Status is one of `success`, `error`,
`wait_response`, `running`, or `pending_compress`; directive is `continue`,
`finish`, or `wait_user`. Planned and started events contain normalized
arguments and optional typed metadata. Completed events contain the outcome
fields and optional typed metadata. Cancellation, process loss, or a panic
after started may leave no completed observation; checkpoint v2's operation
journal, not telemetry, is authoritative for recovery ambiguity.

`ToolLifecycleCallback` and `ToolLifecycleEvent` are exported Rust extension
APIs used by the low-level `ToolOrchestrator` and runtime adapters to observe
`Planned`, `Started`, and `Completed`. They are a language-side observation
adapter for the central lifecycle, not an additional
`vv-agent-contract` semantic. Callback panic is isolated and observation never
changes policy, approval, the tool result, completion, or event-store failure
mode. Product integrations should normally consume `RunEventPayload` and a
`RunEventStore`; the callback is not a durable event stream.

Tool schema wording is part of the agent contract. Changes belong with tests in
`tests/tool_schema_contract.rs`, `tests/tool_planner.rs`, and the closest tool
behavior test.

## Workspace Boundary

Workspace file tools must go through the `WorkspaceBackend` abstraction. Local,
memory, and S3-compatible implementations should keep read/write/list/grep
behavior aligned wherever practical. Path traversal and trusted
outside-workspace access are boundary concerns, not handler-specific shortcuts.

## Invariants

- Requested model keys resolve exactly; independent provider models are not
  aliases for one another.
- Provider HTTP and request serialization stay in `vv-llm`.
- Terminal agent states come from explicit tool directives, declared no-tool
  policy, cancellation, failure, or resource bounds.
- `ToolMetadata::terminal` declares capability only and cannot create a
  terminal state.
- Omitting typed tool metadata and the four metadata-denial fields preserves
  model-visible schemas and existing tool, approval, and completion behavior.
- Runtime hooks, cancellation, streaming, memory compaction, and execution
  backends must compose without changing public result shapes.
- Large tool outputs keep model-facing text and structured metadata separated.
- Public API changes need tests in the closest `tests/*.rs` module.
