# vv-agent-rs Goal

## Objective

Build `vv-agent-rs` as an independent Rust Agent package with complete runtime,
SDK, tool, prompt, memory, skill, workspace, and CLI capabilities.

## Primary Outcome

By the end of this work, `vv-agent-rs/` should be a Rust workspace that:

1. Exposes a main library crate named `vv-agent`.
2. Keeps a clear module hierarchy for these domains:
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

The repository should stay easy to inspect and maintain:

```text
vv-agent-rs/
  crates/vv-agent/
    src/
      runtime/
      tools/
      memory/
      prompt/
      sdk/
      workspace/
```

Prefer domain modules with explicit ownership boundaries over large flattened
files or unrelated abstraction layers.

## Target Capabilities

The Rust package should cover these major Agent areas:

- LLM client configuration and endpoint resolution through `vv-llm`
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
2. The main crate exports a stable public API for the runtime, SDK, tools, workspace, memory, skills, LLM, and protocol layers.
3. The main crate contains a CLI entry point in the same repository, not a standalone CLI crate.
4. Core modules exist for the runtime, SDK, tools, workspace, memory, skills, LLM, and types layers.
5. The implementation passes the repo's Rust checks:
   - `cargo fmt --check`
   - `cargo test`
   - `cargo clippy --all-targets --all-features -- -D warnings`
6. The project includes examples or smoke tests that demonstrate calling the SDK from Rust.
7. Live LLM smoke tests can run through `vv-llm` without custom provider request conversion in this repository.

## Non-Goals

- Do not invent a separate CLI-first architecture.
- Do not split the CLI into an extra workspace crate unless there is a hard technical reason.
- Do not optimize for feature novelty before core Agent runtime completeness.
- Do not treat this as a rewrite of the broader VectorVein repo; this workspace should stay focused on the Agent library.
- Do not put migration notes, source-language rationale, or compatibility explanations in model-facing prompts, tool schemas, Rustdoc, or package docs.

## Implementation Principle

Prefer direct, inspectable domain structure over unnecessary abstraction.

When adding behavior, first place it in the existing domain module that owns the
runtime contract. Add a new abstraction only when it removes real duplication or
keeps a shared contract easier to verify.
