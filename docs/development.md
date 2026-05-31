# Development

Run commands from the `vv-agent-rs/` workspace root.

## Setup

The Rust workspace currently contains the `vv-agent` crate:

```bash
cargo metadata --no-deps
```

For live tests and examples that call real models, create a local settings file
from the checked-in template:

```bash
cp crates/vv-agent/tests/dev_settings.example.json crates/vv-agent/tests/dev_settings.json
```

Fill real endpoint keys in `dev_settings.json`. Do not commit that file. For
examples run from the workspace root, you can also copy the template to
`local_settings.json`.

## Fast Checks

Use targeted checks while iterating:

```bash
cargo test -p vv-agent --test vv_llm_integration
cargo test -p vv-agent --test runtime_cycle
cargo test -p vv-agent --test public_sdk_redesign
cargo test -p vv-agent --test no_legacy_sdk
cargo test -p vv-agent --test workspace_tools
cargo test -p vv-agent --test examples_coverage
```

Run broad checks before reporting a shared behavior change:

```bash
cargo fmt --check
cargo check --examples
cargo test -p vv-agent
```

Run clippy before release-style cleanup or when touching shared abstractions:

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

## Live Tests

Live tests are skipped by default and require real provider credentials:

```bash
VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_deepseek -- --ignored
VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_moonshot -- --ignored
```

Common environment variables:

| Variable | Default | Purpose |
| --- | --- | --- |
| `VV_AGENT_LIVE_SETTINGS_JSON` | `crates/vv-agent/tests/dev_settings.json` | Settings file for live tests. |
| `VV_AGENT_LIVE_MODEL` | test-specific default | Provider model for live smoke tests. |
| `VV_AGENT_RUN_LIVE_TESTS` | unset | Must be `1` to run ignored live tests. |

## Test Ownership

| Change area | Primary tests |
| --- | --- |
| Settings and model resolution | `tests/vv_llm_integration.rs` |
| CLI | `tests/cli.rs` |
| Runtime loop and terminal states | `tests/runtime_cycle.rs`, `tests/cycle_runner.rs` |
| Runtime hooks | `tests/runtime_cycle/hooks.rs` |
| Execution backends and state stores | `tests/runtime_backends.rs`, `tests/state_store.rs` |
| Tools and schemas | `tests/tools_dispatcher.rs`, `tests/tool_schema_contract.rs`, `tests/tool_planner.rs` |
| Workspace tools/backends | `tests/workspace_tools.rs`, `tests/search_tools.rs` |
| Memory and compaction | `tests/memory_tools.rs`, `tests/microcompact.rs`, `tests/post_compact_restore.rs` |
| Agent/Runner API | `tests/public_sdk_redesign.rs`, `tests/sdk_smoke.rs`, `tests/no_legacy_sdk.rs` |
| LLM bridge/streaming/failover | `tests/llm_streaming.rs`, `tests/vv_llm_integration.rs` |
| Skills | `tests/skills_public_api.rs` |
| Examples | `tests/examples_coverage.rs`, `cargo check --examples` |

## Change Hygiene

- Keep public exports in `src/lib.rs` aligned with new public types.
- Update README, examples, and docs when user-facing commands, defaults, or
  environment variables change.
- New embedded SDK examples should use `Agent`, `Runner`, `RunConfig`,
  `ExecutionMode`, `ModelRef`, `ModelSettings`, `FunctionTool`, `ToolOutput`,
  and `Session`.
- Keep key templates checked in and real key files ignored.
- Prefer explicit configuration errors over implicit fallback behavior.
- Keep model-visible schema wording deliberate and covered by contract tests.
- Avoid moving provider-specific HTTP behavior into this crate; add that to
  `vv-llm` instead.
