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
      tools/
        base.rs
        common.rs
        mod.rs
        registry.rs
        schemas.rs
        handlers/
          background.rs
          bash.rs
          control.rs
          image.rs
          memory.rs
          search.rs
          skills.rs
          sub_agents.rs
          workspace_io.rs
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
- `vv-llm = "0.1.0"` backed chat client construction through
  `build_vv_llm_from_local_settings`, settings-based endpoint resolution, and
  provider HTTP/protocol handling delegated to `vv-llm`, while keeping
  `ScriptedLlmClient` for deterministic tests.
- A basic multi-cycle runtime that sends tool schemas to the LLM, executes tool
  calls, and converges on `task_finish` or `ask_user`.
- Split `tools/` modules modeled after Python `v-agent`: `base`, `registry`,
  canonical `schemas`, shared `common` helpers, and focused handler modules.
- Default tool schemas now use reference-quality descriptions derived from
  Python `v-agent` so the model sees the same operational guidance for file
  access, grep, bash/background commands, todos, skills, images, and sub-agents.
- Built-in control tools (`task_finish`, `ask_user`, `todo_write`), core
  workspace tools (`list_files`, `file_info`, `read_file`, `write_file`,
  `file_str_replace`, `workspace_grep`, `read_image`), memory notes through
  `compress_memory`, and `bash` / `check_background_command` command tools with
  captured output, stdin, foreground timeout handoff, and background polling.
- Python-compatible workspace path safety: file, image, grep, and bash tools
  reject paths outside the workspace by default, with metadata-controlled
  outside-path access for trusted tasks.
- Python-compatible `read_file` response limiting: large reads return file
  statistics, request size, limits, and a suggested line range instead of
  flooding the LLM context.
- Sub-agent tool protocol support for `create_sub_task` / `sub_task_status`,
  including injected synchronous runners and batch aggregation.
- Python-style tool planning from `AgentTask` flags, plus `.vv-agent`
  discovery for `agents.json`, prompt templates, and skill directories.
- SDK client, tool registry, workspace backends, and shared protocol types.
- Smoke tests covering public API construction, Rust SDK usage, vv-llm
  integration, runtime tool cycles, schema parity, and workspace tools.

Deeper parity work against the Python implementation is still pending for hooks,
full memory compaction, skills activation, full sub-agent runtime/session
management, session steering, distributed backends, and the remaining built-in
tools. The migration target is to copy Python `v-agent` behavior as completely
as possible, not merely provide a minimal Rust wrapper.
