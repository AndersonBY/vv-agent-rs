# vv-agent Examples

[中文](README_ZH.md)

These examples show the main ways to embed and operate `vv-agent`: Agent +
Runner, direct runtime use, sessions, hooks, custom tools,
sub-agents, streaming, state stores, execution backends, and workspace backends.

Run commands from the `vv-agent-rs` repository root, the directory that
contains `Cargo.toml`:

```bash
cd path/to/vv-agent-rs
```

## Setup

Most examples call a real model through `vv-llm`. By default they read:

- `VV_AGENT_LOCAL_SETTINGS=local_settings.json`
- `VV_AGENT_EXAMPLE_BACKEND=moonshot`
- `VV_AGENT_EXAMPLE_MODEL=kimi-k3`
- `VV_AGENT_EXAMPLE_WORKSPACE=./workspace`
- `VV_AGENT_EXAMPLE_VERBOSE=true`

You can start from the checked-in settings template and keep the real key file
untracked:

```bash
cp crates/vv-agent/tests/dev_settings.example.json local_settings.json
```

Fill the endpoint keys in `local_settings.json`, then run an example:

```bash
VV_AGENT_EXAMPLE_MODEL=kimi-k3 \
cargo run -p vv-agent --example 01_quick_start
```

To use a different settings file:

```bash
VV_AGENT_LOCAL_SETTINGS=crates/vv-agent/tests/dev_settings.json \
VV_AGENT_EXAMPLE_MODEL=kimi-k3 \
cargo run -p vv-agent --example 03_sdk_client
```

## Example Index

| Example | Focus |
| --- | --- |
| `01_quick_start` | Direct runtime setup with prompt construction and tool registry. |
| `02_agent_profiles` | `Agent` profile metadata with `Runner`. |
| `03_sdk_client` | One-shot `Runner` flow with handoff to another `Agent`. |
| `04_session_api` | Long-lived `MemorySession` with `RunConfig`. |
| `05_ask_user_resume` | `ask_user` wait state and resume flow. |
| `06_runtime_hooks` | Before-LLM and before-tool hooks. |
| `07_token_budget_guard` | Public token and tool-call run budgets. |
| `08_custom_tool` | Registering and invoking a custom tool. |
| `09_resource_loader` | Loading agent, prompt, and skill resources from a workspace. |
| `10_read_image` | Multimodal image reading through the `read_image` tool. |
| `11_sub_agent_pipeline` | Coordinated sub-agent pipeline over workspace files. |
| `12_skill_activation` | Skill discovery and `activate_skill` usage. |
| `13_arxiv_pipeline` | Research-style pipeline with budget guard hooks. |
| `14_batch_sub_tasks` | Batch sub-task delegation. |
| `15_memory_compact_hook` | Memory compaction hook behavior. |
| `16_hook_composition` | Combining timing, policy, and result hooks. |
| `17_error_recovery` | Retry wrapper around `Runner` calls. |
| `18_cancellation` | Cancellation token with direct runtime execution. |
| `19_streaming` | Live typed event streaming with `Runner::stream()`. |
| `20_thread_backend` | Thread execution backend. |
| `21_state_checkpoint` | Checkpoint serialization with memory/SQLite stores. |
| `22_sdk_advanced` | Advanced `RunConfig` options with threaded execution. |
| `23_distributed_backend` | Distributed backend API with inline fallback. |
| `24_workspace_backends` | Local, memory, S3-compatible, and wrapped workspace backends. |
| `25_temporary_tool_injection` | Runtime hook that injects a temporary tool window. |
| `26_agent_runner_facade` | `Agent` + `Runner` with `VvLlmModelProvider`. |
| `27_facade_handoff` | Handoff flow that transfers control to another agent. |
| `28_facade_approval_background_trace` | Live approval provider, background agent task, and JSONL trace exporter. |
| `29_typed_final_output` | Deserialize a JSON final output into a Rust type. |

### Add Metadata to the Custom Tool Builder

`08_custom_tool` is the existing `FunctionTool` builder example. A host can
extend the same builder with typed capability metadata and pass cumulative
denials through `RunConfig`:

```rust
use serde::Deserialize;
use serde_json::json;
use vv_agent::{
    FunctionTool, RunConfig, ToolIdempotency, ToolMetadata, ToolOutput, ToolPolicy,
    ToolSideEffect,
};

#[derive(Deserialize)]
struct EchoArgs {
    text: String,
}

let echo = FunctionTool::builder("echo_uppercase")
    .description("Return the provided text uppercased.")
    .tool_metadata(ToolMetadata {
        side_effect: ToolSideEffect::None,
        idempotency: ToolIdempotency::Supported,
        terminal: false,
        capability_tags: vec!["text.transform".to_string()],
        cost_dimensions: Vec::new(),
    })
    .json_schema(json!({
        "type": "object",
        "properties": {"text": {"type": "string"}},
        "required": ["text"]
    }))
    .handler(|_context, args: EchoArgs| async move {
        Ok(ToolOutput::text(args.text.to_uppercase()))
    })
    .build()?;

let config = RunConfig::builder()
    .tool_policy(ToolPolicy::default().deny_terminal_tools())
    .build();
```

The public Rust field is `cost_dimensions` (plural). A policy match reports the
singular source `metadata.cost_dimension`. Neither typed metadata nor the
policy changes the model-visible schema, and omitting both preserves the
existing custom-tool behavior.

Advanced integration: the App Server protocol is documented in
`crates/vv-agent/docs/app_server.md`. It is the supported path for product
shells that need JSON-RPC thread, turn, item, approval, and replay control.

## Verification

Check that all examples compile:

```bash
cargo check --examples
```

Check that the numbered example set is complete:

```bash
cargo test -p vv-agent --test examples_coverage
```

Run the full crate test suite:

```bash
cargo test -p vv-agent
```

The live smoke tests are separate from these examples. See the repository root
README for `VV_AGENT_RUN_LIVE_TESTS` and `crates/vv-agent/tests/dev_settings.json`.
