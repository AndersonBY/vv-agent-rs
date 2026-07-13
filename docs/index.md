# vv-agent-rs Documentation Index

This directory is the source of truth for maintainer- and agent-facing project
knowledge. `AGENTS.md` is intentionally short and points here instead of
duplicating the details.

## Core Documents

| Document | Use it for |
| --- | --- |
| `architecture.md` | Runtime structure, module boundaries, execution flow, and invariants. |
| `parity-contract.md` | Rust producer mapping and local adoption commands for the canonical `vv-agent-contract` release. |
| `development.md` | Local setup, Cargo commands, test ownership, live tests, and change hygiene. |
| `interactive-sessions.md` | Embedded session lifecycle, live control, events, and typed final output. |
| `model-settings.md` | `LLM_SETTINGS`, key-file handling, exact model resolution, defaults, and `vv-llm` boundaries. |
| `runtime-control.md` | Per-run controls, language adaptations, resume, approvals, sessions, cancellation, and event producers. |
| `app_server.md` | JSONL protocol, lifecycle, approval, schema generation, CLI startup, and host boundary. |

## Existing Entry Points

- `README.md` and `README_ZH.md`: user-facing overview and verification guide.
- `crates/vv-agent/examples/README.md`: runnable example catalog.
- `crates/vv-agent/tests/dev_settings.example.json`: checked-in live-test and
  example settings template with placeholder keys.
- `Cargo.toml`: workspace definition.
- `crates/vv-agent/Cargo.toml`: crate metadata, dependencies, features, and
  example targets.
- `crates/vv-agent/tests/`: executable behavior contract for public API,
  runtime, SDK, tools, workspace, memory, model settings, examples, and live
  smoke tests.

## Documentation Maintenance

- Update the narrowest document that owns the changed behavior.
- Keep `AGENTS.md` concise; add details here instead.
- Use repository-relative paths and commands from the workspace root.
- Avoid machine-specific absolute paths in public docs.
- If a documented invariant can drift, point to the test that enforces it or
  add one in the same change.
