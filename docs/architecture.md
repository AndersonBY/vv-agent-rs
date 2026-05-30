# Architecture

`vv-agent-rs` is a Rust workspace for the `vv-agent` crate. The crate provides
an agent runtime, SDK, CLI, built-in tools, memory management, execution
backends, and workspace backends. Provider protocol details are intentionally
delegated to the `vv-llm` crate.

## Top-Level Flow

```text
CLI / SDK / embedding application
  -> config::load_llm_settings_from_file
  -> config::resolve_model_endpoint
  -> llm::VvLlmClient
  -> runtime::AgentRuntime
      -> cycle runner
      -> memory manager
      -> tool planner
      -> tool-call runner
      -> execution backend
  -> AgentResult / AgentRun
```

Task completion is tool-driven. The model must call `task_finish` or `ask_user`
to finish, wait for user input, or continue; the runtime does not infer success
from an assistant prose message.

## Module Map

| Path | Responsibility |
| --- | --- |
| `crates/vv-agent/src/config/` | Settings literal parsing, API key decoding, backend normalization, endpoint lookup, and exact model resolution. |
| `crates/vv-agent/src/cli/` | CLI argument parsing, task construction, runtime logging, and output payloads. |
| `crates/vv-agent/src/types/` | Public protocol types, dictionaries, messages, tasks, statuses, records, and token usage. |
| `crates/vv-agent/src/llm/` | LLM trait, scripted test client, `vv-llm` bridge, endpoint failover, streaming, prompt cache, and request normalization. |
| `crates/vv-agent/src/runtime/` | Agent runtime, cycle execution, hooks, cancellation, shell runtime, background sessions, sub-agents, state stores, and execution backends. |
| `crates/vv-agent/src/tools/` | Tool registry, schemas, dispatcher, shared parsing helpers, and built-in handlers. |
| `crates/vv-agent/src/constants/` | Stable tool names and model-visible schema constants. |
| `crates/vv-agent/src/memory/` | Token counting, compaction, artifact storage, session memory, micro-compaction, prompt-too-long handling, and file-context restoration. |
| `crates/vv-agent/src/prompt/` | System prompt sections, prompt-cache break tracking, available skills, and prompt hashes. |
| `crates/vv-agent/src/sdk/` | High-level client, named agents, resource loading, one-shot runs, query helpers, sessions, and SDK run payloads. |
| `crates/vv-agent/src/workspace/` | Local, memory, and S3-compatible workspace backends. |
| `crates/vv-agent/src/skills/` | Skill discovery, frontmatter parsing, normalization, validation, prompt rendering, and activation state. |

## Execution Backends

- Inline backend: synchronous default execution.
- Thread backend: non-blocking execution with task submission.
- Distributed backend: checkpointed cycle execution with inline fallback and pluggable dispatchers.
- Checkpoint stores: in-memory, SQLite, and Redis.

Distributed and checkpointed paths must preserve the same public result and
checkpoint payload shape as inline execution.

## Tool Boundaries

Tool behavior is split so schemas and handlers can be tested independently:

- `tools/base/`: context, paths, and tool spec/result types.
- `tools/common/`: shared argument, path, grep, edit, and process helpers.
- `tools/handlers/`: concrete built-in tool behavior.
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
