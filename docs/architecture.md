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

Task completion is tool-driven. The model must call `task_finish` or `ask_user`
to finish, wait for user input, or continue; the runtime does not infer success
from an assistant prose message.

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

Distributed mode sends a versioned `DistributedRunEnvelope` for each cycle.
Workers resolve all referenced capabilities before claiming state, then use a
revision/token lease with heartbeat renewal and CAS commit. The scheduler
accepts a result only after reconciling it with the durable checkpoint;
terminal checkpoints are immutable and replayable until acknowledged. SQLite
uses WAL, a bounded busy timeout, and in-place legacy-column migration.

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
  hooks, cancellation, public `ExecutionMode`, providers, event store, and
  metadata override. See `runtime-control.md` for the complete surface.
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
  into the registry-backed executor path.
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
- Terminal agent states are explicit tool outcomes.
- Runtime hooks, cancellation, streaming, memory compaction, and execution
  backends must compose without changing public result shapes.
- Large tool outputs keep model-facing text and structured metadata separated.
- Public API changes need tests in the closest `tests/*.rs` module.
