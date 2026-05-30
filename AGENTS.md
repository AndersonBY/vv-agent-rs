# AGENTS.md

This file is a short map for coding agents. Keep durable project knowledge in
`docs/` and keep this file focused on where to look and what to verify.

## Start Here

- Read `docs/index.md` first when a task touches more than one area.
- Use `docs/architecture.md` for runtime, SDK, memory, tools, backend, and
  workspace boundaries.
- Use `docs/development.md` for setup, cargo commands, test selection, and
  live-test workflow.
- Use `docs/model-settings.md` before changing model defaults, settings
  parsing, examples, live tests, or `vv-llm` integration behavior.
- User-facing examples live in `crates/vv-agent/examples/` and are indexed by
  `crates/vv-agent/examples/README.md`.

## Repository Rules

- Work from the `vv-agent-rs/` workspace root.
- Do not edit or commit real key files. `crates/vv-agent/tests/dev_settings.json`
  is local-only; use `crates/vv-agent/tests/dev_settings.example.json` as the
  checked-in template.
- Do not read key files from sibling projects.
- Do not add aliases between independent provider models. Requested model keys
  must resolve exactly from `LLM_SETTINGS`.
- Provider HTTP details and request serialization belong in `vv-llm`; this
  crate owns agent runtime, SDK, tools, prompts, memory, and workspace behavior.
- Keep README, examples, tests, CLI defaults, and settings templates aligned
  when user-facing defaults change.
- Update `docs/` after significant behavior or workflow changes; keep this file
  as a pointer rather than a long manual.

## Common Commands

```bash
cargo fmt --check
cargo test -p vv-agent
cargo check --examples
cargo clippy --all-targets --all-features -- -D warnings
```

Targeted checks are preferred while iterating:

```bash
cargo test -p vv-agent --test vv_llm_integration
cargo test -p vv-agent --test runtime_cycle
cargo test -p vv-agent --test sdk_resources
cargo test -p vv-agent --test workspace_tools
cargo test -p vv-agent --test examples_coverage
```

Live tests are opt-in and require a local settings file:

```bash
VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_deepseek -- --ignored
VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_moonshot -- --ignored
```

## Change Boundaries

- Settings and model resolution: `crates/vv-agent/src/config/`.
- CLI: `crates/vv-agent/src/cli/`.
- LLM bridge and request normalization: `crates/vv-agent/src/llm/`.
- Runtime orchestration: `crates/vv-agent/src/runtime/`.
- SDK: `crates/vv-agent/src/sdk/`.
- Tools and schemas: `crates/vv-agent/src/tools/` and
  `crates/vv-agent/src/constants/`.
- Memory and compaction: `crates/vv-agent/src/memory/`.
- Workspace backends: `crates/vv-agent/src/workspace/`.
- Skills: `crates/vv-agent/src/skills/`.

When a change crosses these boundaries, add or update tests in the matching
`crates/vv-agent/tests/*.rs` module before reporting completion.
