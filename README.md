# vv-agent-rs

[中文文档](README_ZH.md)

`vv-agent-rs` is the Rust workspace for the `vv-agent` crate: an embeddable
agent runtime, SDK, CLI, tool system, memory layer, and workspace abstraction
for model-driven automation.

It is designed around explicit agent control flow. A task is not considered
done because the model wrote a final-looking sentence; the model must call
`task_finish` to complete or `ask_user` to pause for user input. This keeps
CLI runs, SDK sessions, background runs, and distributed execution on the same
result contract.

## Architecture

```text
AgentRuntime
├── LLM client              # vv-llm backed chat client, endpoint resolution, streaming
├── CycleRunner             # one model turn: prompt, response, tool-call plan
├── ToolCallRunner          # tool dispatch and directive convergence
├── RuntimeHookManager      # before/after hooks for LLM, tools, and memory
├── MemoryManager           # context budgeting, compaction, artifacts, session memory
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

## Setup

Run commands from this repository root:

```bash
cd vv-agent-rs
cargo test -p vv-agent
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
                .execution_mode(ExecutionMode::Inline)
                .build(),
        )
        .await?;
    println!("{:?}", result.final_output());
    Ok(())
}
```

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

### Low-Level Runtime

Use the runtime directly only when you need to assemble the LLM client, prompt,
tool registry, workspace, and run controls yourself. New embedded applications
should start with `Agent` + `Runner`.

```rust
use std::path::PathBuf;

use vv_agent::config::build_vv_llm_from_local_settings;
use vv_agent::prompt::{build_system_prompt_with_options, BuildSystemPromptOptions};
use vv_agent::{build_default_registry, AgentRuntime, AgentTask, RuntimeRunControls};

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
| Runtime | Multi-cycle model execution, tool planning, explicit terminal states, cancellation, streaming, event logs, and max-cycle handling. |
| Tools | Built-in tools for finish/wait-user, TODOs, workspace reads/writes/listing/grep, image reads, shell commands, memory notes, skills, and sub-tasks. |
| SDK | `Agent`, `Runner`, `RunConfig`, `ModelSettings`, typed tools, `Agent::as_tool()`, typed events, and `Session`. |
| Memory | Token budgeting, prompt-too-long retries, micro and full compaction, artifact-backed large tool results, image trimming, and session memory. |
| Hooks | Rust `RuntimeHook` implementations can inspect or patch LLM calls, tool calls, memory compaction, and run lifecycle behavior. |
| Sub-agents | Runtime-backed sub-task creation, batch submission, background status polling, continuation, steering, and inherited streaming callbacks. |
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

`list_files` and `workspace_grep` include safety defaults for large workspaces:
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
Agent + Runner, runtime hooks, custom tools, handoffs, approval resume,
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
```

The live suite exercises direct runtime completion, SDK completion,
`ask_user`, TODO updates, memory notes, skill activation, workspace tools,
image reading, foreground and background shell commands, sub-agent polling, and
configured sub-agent delegation.

## Verification

Run the standard checks from `vv-agent-rs/`:

```bash
cargo fmt --check
cargo test -p vv-agent
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
