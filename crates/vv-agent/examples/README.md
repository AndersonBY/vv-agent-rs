# vv-agent Examples

[中文](README_ZH.md)

These examples show the main ways to embed and operate `vv-agent`: direct
runtime use, SDK clients, sessions, hooks, custom tools, sub-agents, streaming,
state stores, execution backends, and workspace backends.

Run commands from the `vv-agent-rs` repository root, the directory that
contains `Cargo.toml`:

```bash
cd path/to/vv-agent-rs
```

## Setup

Most examples call a real model through `vv-llm`. By default they read:

- `VV_AGENT_LOCAL_SETTINGS=local_settings.json`
- `V_AGENT_EXAMPLE_BACKEND=moonshot`
- `V_AGENT_EXAMPLE_MODEL=kimi-k2.6`
- `V_AGENT_EXAMPLE_WORKSPACE=./workspace`
- `V_AGENT_EXAMPLE_VERBOSE=true`

You can start from the checked-in settings template and keep the real key file
untracked:

```bash
cp crates/vv-agent/tests/dev_settings.example.json local_settings.json
```

Fill the endpoint keys in `local_settings.json`, then run an example:

```bash
V_AGENT_EXAMPLE_MODEL=kimi-k2.6 \
cargo run -p vv-agent --example 01_quick_start
```

To use a different settings file:

```bash
VV_AGENT_LOCAL_SETTINGS=crates/vv-agent/tests/dev_settings.json \
V_AGENT_EXAMPLE_MODEL=kimi-k2.6 \
cargo run -p vv-agent --example 03_sdk_client
```

## Example Index

| Example | Focus |
| --- | --- |
| `01_quick_start` | Direct runtime setup with prompt construction and tool registry. |
| `02_agent_profiles` | SDK client with named agent profiles. |
| `03_sdk_client` | One-shot SDK run/query flow with configured sub-agents. |
| `04_session_api` | Long-lived SDK session lifecycle. |
| `05_ask_user_resume` | `ask_user` wait state and resume flow. |
| `06_runtime_hooks` | Before-LLM and before-tool hooks. |
| `07_token_budget_guard` | Token budget monitoring and forced finish behavior. |
| `08_custom_tool` | Registering and invoking a custom tool. |
| `09_resource_loader` | Loading agent, prompt, and skill resources from a workspace. |
| `10_read_image` | Multimodal image reading through the `read_image` tool. |
| `11_sub_agent_pipeline` | Coordinated sub-agent pipeline over workspace files. |
| `12_skill_activation` | Skill discovery and `activate_skill` usage. |
| `13_arxiv_pipeline` | Research-style pipeline with budget guard hooks. |
| `14_batch_sub_tasks` | Batch sub-task delegation. |
| `15_memory_compact_hook` | Memory compaction hook behavior. |
| `16_hook_composition` | Combining timing, policy, and result hooks. |
| `17_error_recovery` | Retry wrapper around SDK runs. |
| `18_cancellation` | Cancellation token with direct runtime execution. |
| `19_streaming` | Streaming callback collection. |
| `20_thread_backend` | Thread execution backend. |
| `21_state_checkpoint` | Checkpoint serialization with memory/SQLite stores. |
| `22_sdk_advanced` | Advanced SDK options with streaming and thread backend. |
| `23_distributed_backend` | Distributed backend API with inline fallback. |
| `24_workspace_backends` | Local, memory, S3-compatible, and wrapped workspace backends. |
| `25_temporary_tool_injection` | Runtime hook that injects a temporary tool window. |

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
