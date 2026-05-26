# vv-agent-rs

Rust workspace for the VectorVein agent library. This crate mirrors the Python
`v-agent/src/vv_agent` public surface closely enough for Rust callers to depend
on a stable top-level API while runtime parity is implemented module by module.

## Layout

```text
vv-agent-rs/
  Cargo.toml
  crates/vv-agent/
    src/
      config.rs
      constants.rs
      integrations.rs
      llm.rs
      memory.rs
      prompt.rs
      runtime.rs
      sdk.rs
      skills.rs
      tools.rs
      types.rs
      workspace.rs
      cli.rs
      main.rs
    tests/
      public_api.rs
      runtime_cycle.rs
      sdk_smoke.rs
      vv_llm_integration.rs
      workspace_tools.rs
```

The package is named `vv-agent`; the library target is imported as `vv_agent`,
matching Rust crate naming rules for hyphenated package names.

## Verification

Run from `vv-agent-rs/`:

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Live DeepSeek smoke tests are opt-in and use the local vv-llm development
settings file without printing credentials:

```bash
VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_deepseek -- --ignored
```

## Current Scope

The current Rust implementation includes:

- A valid Cargo workspace with a main `vv-agent` package.
- A library target exposing top-level API types and functions comparable to
  Python's `vv_agent.__init__`.
- A CLI target inside the same package.
- Top-level modules aligned with the Python package: `background_sessions`,
  `cli`, `config`, `constants`, `integrations`, `llm`, `memory`, `processes`,
  `prompt`, `runtime`, `sdk`, `skills`, `tools`, `types`, and `workspace`.
- `vv-llm` backed chat client construction and settings-based endpoint
  resolution, while keeping `ScriptedLlmClient` for deterministic tests.
- A basic multi-cycle runtime that sends tool schemas to the LLM, executes tool
  calls, and converges on `task_finish` or `ask_user`.
- Built-in control tools (`task_finish`, `ask_user`, `todo_write`), core
  workspace tools (`list_files`, `file_info`, `read_file`, `write_file`,
  `file_str_replace`, `workspace_grep`, `read_image`), memory notes through
  `compress_memory`, and `bash` / `check_background_command` command tools with
  captured output, stdin, foreground timeout handoff, and background polling.
- Sub-agent tool protocol support for `create_sub_task` / `sub_task_status`,
  including injected synchronous runners and batch aggregation.
- SDK client, tool registry, workspace backends, and shared protocol types.
- Smoke tests covering public API construction, Rust SDK usage, vv-llm
  integration, runtime tool cycles, and workspace tools.

Deeper parity work against the Python implementation is still pending for hooks,
full memory compaction, skills activation, full sub-agent runtime/session
management, session steering, distributed backends, and the remaining built-in
tools.
