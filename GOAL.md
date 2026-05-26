# vv-agent-rs Goal

## Objective

Build a Rust implementation of `v-agent` that mirrors the Python project layout and public surface closely enough that the two codebases can be developed and merged side by side.

## Primary Outcome

By the end of this work, `vv-agent-rs/` should be a Rust workspace that:

1. Exposes a main library crate named `vv-agent`.
2. Mirrors the Python package structure with the same conceptual modules:
   - `config`
   - `constants`
   - `integrations`
   - `llm`
   - `memory`
   - `prompt`
   - `runtime`
   - `sdk`
   - `skills`
   - `tools`
   - `types`
   - `workspace`
   - `cli`
3. Keeps the CLI as an entry point inside the main crate, not as a separate top-level crate.
4. Provides a Rust API that can be used from another Rust project without importing internal implementation details.

## Layout Constraint

The project should stay easy to compare with the Python repo:

```text
v-agent/        # Python reference
vv-agent-rs/    # Rust implementation
```

The Rust workspace should prefer module and package names that line up with the Python repo rather than introducing unrelated abstractions.

## Target Capabilities

The Rust version should eventually cover the same major runtime areas as Python:

- LLM client configuration and endpoint resolution
- agent runtime and cycle execution
- tool registry and built-in tools
- workspace backends
- memory / compaction logic
- skills parsing and validation
- SDK-style client API
- CLI entry point

## Quantifiable Completion Criteria

This goal is not done until all of the following are true:

1. `vv-agent-rs/` contains a valid Cargo workspace.
2. The main crate exports a stable public API comparable to Python's top-level `vv_agent.__init__`.
3. The main crate contains a CLI entry point in the same repository, not a standalone CLI crate.
4. Core modules exist for the runtime, SDK, tools, workspace, memory, skills, LLM, and types layers.
5. The first implementation passes the repo's Rust checks:
   - `cargo fmt --check`
   - `cargo test`
   - `cargo clippy --all-targets --all-features -- -D warnings`
6. The project includes at least one example or smoke test that demonstrates calling the SDK from Rust.

## Non-Goals

- Do not invent a separate CLI-first architecture that diverges from Python.
- Do not split the CLI into an extra workspace crate unless there is a hard technical reason.
- Do not optimize for feature novelty before parity with Python's structure.
- Do not treat this as a rewrite of the broader VectorVein repo; this workspace should stay focused on the agent library.

## Implementation Principle

Prefer direct structural correspondence over abstraction.

If the Python repo has a module, the Rust repo should first try to model that module directly before introducing any new layering.
