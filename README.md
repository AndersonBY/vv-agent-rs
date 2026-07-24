# vv-agent-rs

[中文文档](README_ZH.md)

`vv-agent-rs` is the Rust workspace for the `vv-agent` crate: an embeddable
agent runtime, SDK, CLI, tool system, memory layer, and workspace abstraction
for model-driven automation.

## Install

The current stable release is `0.8.0`. It implements language-neutral Contract
`3.0.0` while keeping a Rust-idiomatic API; the sibling implementation consumes
the same contract.

```bash
cargo add vv-agent@0.8.0
```

Enable the Apalis adapter with:

```bash
cargo add vv-agent@0.8.0 --features apalis
```

Contract 3 and repository `HEAD` are forward-only: current readers accept only
the current strict public and wire shapes. Pin an older crate release when an
application must retain an older protocol.

### 0.8.0 Highlights

- Every admitted model dispatch is recorded in
  `result.token_usage().model_calls`, including agent cycles, Session Memory,
  full memory compaction, failures, retries, and ambiguous outcomes. Missing
  provider token or cache fields remain unavailable instead of being reported
  as zero.
- Tool arguments are validated as a complete JSON Schema Draft 2020-12 value
  before approval or side effects. Invalid calls return structured
  `invalid_tool_arguments` details without invoking the handler.
- Optional host output validation is disabled by default and can make at most
  one tools-free repair callback before a terminal result is committed.
- Durable execution uses `vv-agent.checkpoint.v3`,
  `vv-agent.run-definition.v2`, `vv-agent.distributed-run.v2`, and
  `vv-agent.distributed-worker-response.v1` for strict recovery and
  distributed-controller boundaries.

See [output validation](docs/output-validation.md) and
[checkpoint/resume](docs/checkpoint-resume.md) for the detailed contracts.

It is designed around explicit agent control flow. The default uses
`task_finish` to complete and `ask_user` to pause. Hosts can
instead opt into `NoToolPolicy::Finish` or `NoToolPolicy::WaitUser` when a
normal assistant response should be terminal. The runtime applies the declared
policy without classifying whether the text looks like a final answer.

## Architecture

```text
AgentRuntime
├── LLM client              # vv-llm backed chat client, endpoint resolution, streaming
├── CycleRunner             # one model turn: prompt, response, tool-call plan
├── ToolOrchestrator        # tool policy, approval, dispatch, timeout, telemetry
├── RuntimeHookManager      # before/after hooks for LLM, tools, and memory
├── MemoryManager           # context budgeting, compaction, artifacts, session memory
├── RunHandle / RunEvent    # live control, typed events, event-store replay
├── RuntimeExecutionBackend # run scheduling
│   ├── InlineBackend       # synchronous default
│   ├── ThreadBackend       # non-blocking task submission
│   └── DistributedBackend  # checkpointed cycles with pluggable dispatch
└── WorkspaceBackend        # file/object I/O boundary for tools
    ├── LocalWorkspaceBackend
    ├── MemoryWorkspaceBackend
    └── S3WorkspaceBackend
```

Provider request building, endpoint transport, retries, streaming deltas, token
limits, usage accounting, and provider-specific protocol details are delegated
to the published `vv-llm` crate. `vv-agent` focuses on agent execution: prompts,
tools, hooks, memory, sessions, workspace access, and orchestration.

## Repository Setup

Run commands from this repository root:

```bash
cd vv-agent-rs
cargo test -p vv-agent -- --test-threads=1
```

Most real-model examples and the CLI read a local `vv-llm` settings file. Keep
the credential-bearing file untracked:

```bash
cp crates/vv-agent/tests/dev_settings.example.json local_settings.json
# Fill endpoint keys in local_settings.json.
```

The default settings path is `local_settings.json`. You can override it with
`VV_AGENT_LOCAL_SETTINGS` for examples or `--settings-file` for the CLI.

## Quick Start

### CLI

```bash
cargo run -p vv-agent -- \
  --prompt "Summarize this repository" \
  --backend deepseek \
  --model deepseek-v4-pro \
  --settings-file local_settings.json \
  --workspace ./workspace \
  --verbose
```

CLI flags:

| Flag | Purpose |
| --- | --- |
| `--prompt` | Required user task. |
| `--backend` | Backend key under `LLM_SETTINGS.backends`. |
| `--model` | Model key under the selected backend. |
| `--settings-file` | Local `vv-llm` settings file. |
| `--workspace` | Directory exposed to workspace tools. |
| `--max-cycles` | Maximum runtime cycles before stopping. |
| `--language` | Prompt/tool guidance locale. |
| `--agent-type` | Optional agent profile type such as `computer`. |
| `--verbose` | Emit per-cycle runtime events. |

### Agent + Runner SDK

Use `Agent` + `Runner` for new embedded applications. `Agent`
describes instructions, model, tools, handoffs, hooks, and defaults. `Runner`
owns model providers, workspace defaults, and execution. `RunConfig` overrides
one run without changing the agent definition, including the public
`ExecutionMode` for inline, threaded, or distributed execution.
Per-run controls also cover tool registry factories, before-cycle and
interruption messages, sub-task management, runtime observers, log previews,
and LLM request debug dumps. See `docs/runtime-control.md` for precedence and
language-adaptation details.

```rust
use vv_agent::{Agent, ExecutionMode, ModelRef, Runner, RunConfig, VvLlmModelProvider};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let provider = VvLlmModelProvider::from_settings_file("local_settings.json")
        .with_default_backend("deepseek");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()?;

    let agent = Agent::builder("assistant")
        .instructions("You plan, use tools when useful, and call task_finish when done.")
        .model(ModelRef::backend("deepseek", "deepseek-v4-pro"))
        .build()?;

    let result = runner
        .run_with_config(
            &agent,
            "Create notes.md with three project takeaways.",
            RunConfig::builder()
                .max_cycles(12)
                .max_handoffs(4)
                .execution_mode(ExecutionMode::Inline)
                .build(),
        )
        .await?;
    println!("{:?}", result.final_output());
    Ok(())
}
```

A handoff is an outer Runner control transfer, not an agent-as-tool call. The
target Agent resolves its own model and model settings, while the active
session, cancellation token, and mutated shared state continue across the
transition. `max_handoffs` defaults to `10` and limits control transfers
independently from `max_cycles`. Approval resume preserves the same behavior.

No-tool completion is an explicit host control. Configure it on an Agent or
override it for one run; per-run configuration wins over a configured Runner
default, which wins over the Agent value. Omitting every layer keeps
`NoToolPolicy::Continue`.

```rust
use vv_agent::{Agent, NoToolPolicy, RunConfig};

let natural_answer_agent = Agent::builder("assistant")
    .instructions("Answer from the available context.")
    .no_tool_policy(NoToolPolicy::Finish)
    .build()?;
let force_tool_driven_run = RunConfig::builder()
    .no_tool_policy(NoToolPolicy::Continue)
    .build();
```

Inspect `RunResult::completion_reason()`, `completion_tool_name()`, and
`partial_output()` to distinguish natural completion, tool-driven completion,
waits, cancellation, failure, and max-cycle exhaustion.

Reusable run defaults belong in
`Runner::builder().default_run_config(...)`. Provider resolution is per-run
then Runner. Model resolution is per-run, Agent, Runner, then the selected
provider default. Model settings merge from provider to Runner to Agent to
per-run, with each later layer overriding earlier fields. Replacing the
provider for one run does not reuse a backend-bound model from the Runner.

Sessions keep conversation history across runner calls:

```rust
use vv_agent::{MemorySession, RunConfig};

let session = MemorySession::new("thread-001");
runner
    .run_with_config(&agent, "Analyze the current workspace.", RunConfig::builder().session(session.clone()).build())
    .await?;
let result = runner
    .run_with_config(&agent, "Continue with follow-up suggestions.", RunConfig::builder().session(session).build())
    .await?;
```

### Live Runs and Events

`Runner::run()` and `run_with_config()` are the one-shot entrypoints. Use
`Runner::start()` when an application needs live UI/server control: subscribe
to events, approve pending tools, cancel a run, or await the final result from
one `RunHandle`. `Runner::stream()` is a convenience wrapper over `start()` for
typed live events.

```rust
use vv_agent::{ApprovalDecision, RunConfig, RunEventPayload};

let handle = runner
    .start(&agent, "Inspect the workspace and report findings.", RunConfig::default())
    .await?;
let mut events = handle.events();

while let Some(event) = events.next().await {
    match event?.payload() {
        RunEventPayload::AssistantDelta { delta } => print!("{delta}"),
        RunEventPayload::ToolCallStarted { tool_name, .. } => {
            eprintln!("tool started: {tool_name}");
        }
        RunEventPayload::ApprovalRequested { request_id, .. } => {
            handle.approve(request_id, ApprovalDecision::allow()).await?;
        }
        _ => {}
    }
}

let result = handle.result().await?;
```

Each `RunEvent` is a v1 envelope with `event_id`, `run_id`, `trace_id`,
optional session and parent identifiers, timing, metadata, and a typed
`RunEventPayload`. `JsonlRunEventStore` can append events and replay a run,
including child events linked by parent run id.

Live tool approval uses `ApprovalProvider` and the handle-owned broker. The
model-facing `ask_user` tool remains for requesting user input as part of the
conversation. Host applications can also attach `ContextProvider` values for
ordered prompt fragments and `MemoryProvider` values for external search, save,
and compaction lifecycle hooks.

`ToolPolicy` exposes `Default`, `Always`, `Never`, and `OnRequest` approval
modes. `Default` inherits the next configured policy; explicit `OnRequest`
follows each tool's static or dynamic approval declaration. `Always` forces
approval and `Never` bypasses it without evaluating a dynamic tool predicate.

### Tool Metadata and Execution Telemetry

Tools may declare optional, host-visible capabilities with `ToolMetadata`.
Attach the declaration with
`FunctionTool::builder(...).tool_metadata(...)` (or
`StaticTool::with_tool_metadata`) and narrow a run with the additive denial
methods on `ToolPolicy`:

```rust
use serde_json::Value;
use vv_agent::{
    FunctionTool, RunConfig, ToolIdempotency, ToolMetadata, ToolOutput, ToolPolicy,
    ToolSideEffect,
};

let inspect = FunctionTool::builder("inspect_source")
    .description("Inspect a source file.")
    .tool_metadata(ToolMetadata {
        side_effect: ToolSideEffect::Read,
        idempotency: ToolIdempotency::Supported,
        terminal: false,
        capability_tags: vec!["source.inspect".to_string()],
        cost_dimensions: vec!["workspace.bytes_read".to_string()],
    })
    .handler(|_context, _arguments: Value| async {
        Ok(ToolOutput::text("inspection complete"))
    })
    .build()?;

let policy = ToolPolicy::default()
    .deny_side_effect(ToolSideEffect::Write)
    .deny_capability_tag("secrets.read")?
    .deny_terminal_tools()
    .deny_cost_dimension("workflow.credit")?;
let run_config = RunConfig::builder().tool_policy(policy).build();
```

`side_effect` is one coarse declaration with no hierarchy. `terminal=true`
only declares that a tool may return `finish` or `wait_user`; it never ends a
run by itself. `capability_tags` and `cost_dimensions` are normalized,
exact-match labels. Cost dimensions are not prices, usage measurements, or run
budgets. Typed declarations remain separate from generic tool `metadata` and
are never added to the model-visible function schema.

Metadata denials compose with existing name, argument, approval, planned-name,
budget, and runtime checks. Agent, Runner-default, and per-run denials form a
set union (`deny_terminal_tools` uses logical OR); configured sub-agents,
agent-as-tool runs, handoffs, and distributed workers inherit them and can only
add denials. A matching denial returns `tool_not_allowed` without starting the
executor. Omitting typed metadata and leaving the four new policy fields at
their defaults preserves existing tool eligibility, schemas, completion, and
approval behavior.

The typed runtime sequence is `ToolCallPlanned`, optional approval events,
`ToolCallStarted` immediately before effects may begin, and
`ToolCallCompleted` after a result exists. Completed events expose `directive`,
`error_code`, `execution_started`, and `duration_ms`; a pre-execution denial has
no started event, `execution_started=false`, and `duration_ms=null` on the
wire. See [Architecture](docs/architecture.md),
[Durable Checkpoint And Resume](docs/checkpoint-resume.md), and the
[App Server protocol](crates/vv-agent/docs/app_server.md) for lifecycle,
persistence, and projection details.

### Run Budgets

`RunConfig::budget_limits` can independently limit total tokens, uncached input
tokens, total or exact-name tool calls, active wall time, and host-metered cost.
All limits are optional and task-neutral: the framework does not inspect the
prompt, task category, milestones, or answer quality when enforcing them.

Inspect `result.budget_usage()` and `result.budget_exhaustion()`. A budget stop
is a typed failed result with completion reason `budget_exhausted`, not a
successful answer. Runs without configured limits preserve the existing event
flow. See [Run Budgets](docs/run-budgets.md) and
`crates/vv-agent/examples/07_token_budget_guard.rs`.

### App Server

Use the App Server when a product shell needs to drive `vv-agent` over a stable
JSON-RPC protocol instead of linking directly to runtime internals. It supports
stdio JSONL transport, thread and turn lifecycle requests, live item
notifications, approval server requests, replay, schema generation, and a typed
Rust test client.

```bash
vv-agent app-server --listen stdio
vv-agent app-server schema --out target/app-server-schema/json
vv-agent app-server generate-ts --out target/app-server-schema/typescript
```

See `crates/vv-agent/docs/app_server.md` for protocol examples and client
responsibilities.

### Low-Level Runtime

Use the runtime directly only when you need to assemble the LLM client, prompt,
tool registry, workspace, and run controls yourself. New embedded applications
should start with `Agent` + `Runner`.

```rust
use std::path::PathBuf;

use vv_agent::config::build_vv_llm_from_local_settings;
use vv_agent::prompt::{build_system_prompt_with_options, BuildSystemPromptOptions};
use vv_agent::types::AgentTask;
use vv_agent::{build_default_registry, AgentRuntime, RuntimeRunControls};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (llm, resolved) = build_vv_llm_from_local_settings(
        "local_settings.json",
        "deepseek",
        "deepseek-v4-pro",
        90.0,
    )?;
    let runtime = AgentRuntime::new(llm).with_tool_registry(build_default_registry());
    let system_prompt = build_system_prompt_with_options(
        "You are a reliable execution agent.",
        BuildSystemPromptOptions {
            language: "zh-CN".to_string(),
            use_workspace: true,
            enable_todo_management: true,
            ..BuildSystemPromptOptions::default()
        },
    );

    let mut task = AgentTask::new(
        "demo",
        resolved.model_id,
        system_prompt,
        "Read the workspace README and summarize the project.",
    );
    task.max_cycles = 12;

    let result = runtime.run_with_controls(
        task,
        RuntimeRunControls {
            workspace: Some(PathBuf::from("./workspace")),
            ..RuntimeRunControls::default()
        },
    )?;
    println!("{:?}: {:?}", result.status, result.final_answer);
    Ok(())
}
```

See `crates/vv-agent/examples/01_quick_start.rs` for a complete low-level
runtime version with event logging.

## Core Capabilities

| Area | What `vv-agent` provides |
| --- | --- |
| Runtime | Multi-cycle model execution, explicit terminal states, live `RunHandle`, cancellation, typed events, event replay, and max-cycle handling. |
| Tools | Built-in tools plus a `ToolOrchestrator` path for policy, approval, dispatch, timeout, and telemetry. |
| SDK | `Agent`, `Runner`, `RunConfig`, `ModelSettings`, typed tools, `Agent::as_tool()`, `RunEvent`, providers, and `Session`. |
| Memory | Token budgeting, prompt-too-long retries, micro and full compaction, artifact-backed large tool results, image trimming, session memory, and external provider hooks. |
| Hooks | Rust `RuntimeHook` implementations can inspect or patch LLM calls, tool calls, memory compaction, and run lifecycle behavior. |
| Sub-agents | Runtime-backed sub-task creation, batch submission, background status queries with wait-for-completion support, continuation, steering, and inherited streaming callbacks. |
| Skills | Skill directory discovery, frontmatter parsing, validation, prompt rendering with budget limits, activation, and activation history. |
| Workspace | Local, in-memory, and S3 object-store backends behind one `WorkspaceBackend` boundary. |

## Execution Backends

The public SDK selects scheduling through `ExecutionMode`. Lower-level runtime
backend structs remain available for advanced integrations:

| Backend | Use case |
| --- | --- |
| `ExecutionMode::Inline` | Default synchronous execution in the current process. |
| `ExecutionMode::Threaded` | Submit runs without blocking the caller. |
| `ExecutionMode::Distributed` | Checkpointed cycle execution with serializable runtime recipes and pluggable dispatch. |

Checkpointed runs can store state in memory, SQLite, or Redis. The optional
`apalis` feature adds an Apalis job bridge for applications that already use
Apalis workers:

```bash
cargo test -p vv-agent --features apalis --test apalis_backend
```

The distributed API also has an inline fallback, which is useful for local
development and tests. See `crates/vv-agent/examples/23_distributed_backend.rs`.

## Workspace Backends

All built-in file tools go through `WorkspaceBackend`. That keeps local files,
memory-backed files, and S3-compatible object storage on the same tool contract.

`find_files` and `search_files` include safety defaults for large workspaces:
bounded result counts, hidden/dependency directory filtering, explicit ignored
path inclusion, and local `rg` acceleration when available.

## Examples

The numbered examples are the best way to explore the public API:

```bash
cargo run -p vv-agent --example 01_quick_start
cargo run -p vv-agent --example 03_sdk_client
cargo run -p vv-agent --example 04_session_api
cargo run -p vv-agent --example 23_distributed_backend
cargo run -p vv-agent --example 24_workspace_backends
cargo run -p vv-agent --example 26_agent_runner_facade
cargo run -p vv-agent --example 27_facade_handoff
cargo run -p vv-agent --example 28_facade_approval_background_trace
```

See `crates/vv-agent/examples/README.md` for the full example index covering
Agent + Runner, runtime hooks, custom tools, handoffs, live approval,
background tasks, tracing, sub-agent pipelines, skills, streaming, cancellation,
state stores, execution backends, workspace backends, and temporary tool
injection.

## Live Smoke Tests

Live tests are opt-in and use a local settings file without printing
credentials. By default they read the untracked
`crates/vv-agent/tests/dev_settings.json`; start from
`crates/vv-agent/tests/dev_settings.example.json`.

```bash
VV_AGENT_RUN_LIVE_TESTS=1 \
cargo test -p vv-agent --test live_deepseek -- --ignored

VV_AGENT_RUN_LIVE_TESTS=1 \
cargo test -p vv-agent --test live_edit_file -- --ignored --test-threads=1
```

The live suite exercises direct runtime completion, SDK completion,
`ask_user`, todo updates, memory notes, skill activation, workspace tools,
image reading, safe `edit_file` recovery, foreground and background shell
commands, sub-agent waiting/status checks, and configured sub-agent delegation.

## Verification

Run the standard checks from `vv-agent-rs/`:

```bash
cargo fmt --check
cargo test -p vv-agent -- --test-threads=1
cargo check --examples
cargo clippy --all-targets --all-features -- -D warnings
```

Focused checks that are useful while editing public docs and examples:

```bash
cargo test -p vv-agent --test public_api
cargo test -p vv-agent --test examples_coverage
```

## Repository Layout

```text
vv-agent-rs/
  Cargo.toml
  crates/vv-agent/
    src/
      cli/        # CLI entrypoint and task construction
      config/     # LLM settings loading and model resolution
      llm/        # LLM trait, scripted test client, vv-llm client bridge
      memory/     # compaction, artifacts, session memory, token budgeting
      prompt/     # system prompt sections and prompt-cache metadata
      agent.rs    # public Agent builder
      runner.rs   # public Runner over runtime execution
      run_config.rs
      model.rs
      model_settings.rs
      sessions.rs
      runtime/    # agent runtime, hooks, backends, cancellation, sub-agents
      skills/     # skill discovery, parsing, validation, activation
      tools/      # registry, schemas, dispatcher, built-in handlers
      workspace/  # local, memory, and S3 workspace backends
    examples/
    tests/
  docs/
```

Additional design notes live under `docs/`, especially `docs/architecture.md`
and `docs/model-settings.md`.
