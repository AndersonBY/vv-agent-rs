# vv-agent-rs

Rust workspace for the VectorVein agent runtime, SDK, CLI, built-in tools, and
workspace backends. The crate is intended to be useful as an independent Rust
package: model-facing prompts and tool schemas describe what the agent can do,
without exposing implementation-history notes.

## Layout

```text
vv-agent-rs/
  Cargo.toml
  crates/vv-agent/
    src/
      config.rs
      constants/
        mod.rs
        tool_names.rs
        workspace.rs
      integrations/
        mod.rs
        protocols.rs
      llm/
        anthropic_prompt_cache.rs
        base.rs
        mod.rs
        scripted.rs
        vv_llm_client.rs
      memory/
        artifacts.rs
        manager.rs
        microcompact.rs
        mod.rs
        session.rs
        session_memory.rs
        summary.rs
        token_utils.rs
      prompt/
        builder.rs
        cache_tracker.rs
        mod.rs
        templates.rs
      runtime/
        backends/
          base.rs
          celery.rs
          celery_tasks.rs
          inline.rs
          mod.rs
          thread.rs
        background_sessions.rs
        cancellation.rs
        context.rs
        engine.rs
        hooks.rs
        mod.rs
        processes.rs
        results.rs
        state.rs
        stores/
          mod.rs
          redis.rs
          sqlite.rs
        sub_agents.rs
        sub_task_manager.rs
        token_usage.rs
        tool_planner.rs
      sdk/
        client.rs
        mod.rs
        python_hooks.rs
        resources.rs
        session.rs
        types.rs
      skills/
        errors.rs
        mod.rs
        models.rs
        normalize.rs
        parser.rs
        prompt.rs
        validator.rs
      tools/
        base.rs
        builtins.rs
        common.rs
        dispatcher.rs
        mod.rs
        registry.rs
        schemas/
          command.rs
          control.rs
          media.rs
          memory.rs
          mod.rs
          sub_agents.rs
          todo.rs
          workspace/
            edit.rs
            file_io.rs
            listing.rs
            mod.rs
            search.rs
        handlers/
          background.rs
          bash.rs
          common.rs
          control.rs
          image.rs
          memory.rs
          search.rs
          skills/
            mod.rs
            state.rs
          sub_agents.rs
          workspace_io.rs
      types.rs
      workspace/
        base.rs
        local.rs
        memory.rs
        mod.rs
        s3.rs
      cli.rs
      main.rs
    tests/
      cli.rs
      control_tools.rs
      live_deepseek.rs
      prompt_public_api.rs
      runtime_cycle.rs
      sdk_resources.rs
      sdk_smoke.rs
      tool_planner.rs
      tool_schema_parity.rs
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

Live DeepSeek smoke tests are opt-in and use a local vv-llm settings file
without printing credentials:

```bash
VV_AGENT_RUN_LIVE_TESTS=1 \
VV_AGENT_LIVE_SETTINGS_JSON=/path/to/dev_settings.json \
cargo test --test live_deepseek -- --ignored
```

## Current Scope

The current Rust implementation includes:

- A Cargo workspace with the main `vv-agent` library and an in-crate
  `vv-agent` CLI.
- Stable crate-level exports for core agent types, runtime execution,
  tool dispatch, built-in tool registration, SDK clients, workspace backends,
  prompt helpers, memory helpers, and shared protocol types.
- `vv-llm = "0.2.0"` based chat client construction through local settings,
  endpoint resolution, endpoint retry/failover, streaming events, prompt-cache
  metadata, request debug dumps, resolved token limits, and usage accounting.
  Provider HTTP and request serialization stay delegated to `vv-llm`.
- A deterministic `ScriptedLlmClient` for tests, including fixed response
  steps, callback response steps, live request inspection, and explicit script
  exhaustion errors.
- Multi-cycle runtime execution with tool-schema planning, tool-call dispatch,
  completion/wait-user convergence, runtime hooks, cancellation, lifecycle
  events, before-cycle message injection, interruption steering, and
  max-cycle handling.
- Execution backends for inline, thread, and checkpoint-dispatched runs, with
  serializable runtime recipes and state stores backed by memory, SQLite, or
  Redis.
- Prompt building with structured sections, stable prompt hashes, localized
  tool guidance, available skill rendering, sub-agent guidance, prompt-cache
  break tracking, current-time sections, and session-memory injection.
- Memory management for context budgeting, usage estimation, artifact-backed
  large tool-result compaction, microcompaction, full summaries, image-payload
  trimming, repeated compaction, session memory, prompt-too-long retries, and
  post-compaction file-context restoration.
- High-information built-in tool schemas and handlers for task completion,
  user questions, TODO management, file listing, metadata lookup, text reads,
  writes, string replacement, grep, image reads, memory notes, foreground and
  background shell commands, skill activation, sub-task creation, and sub-task
  status/continuation.
- Workspace safety and workspace backends for local files, in-memory files, and
  S3-compatible object stores, including deterministic path rendering, glob
  listing, append support, metadata lookup, missing-file errors, hidden/ignored
  filtering, and optional trusted outside-workspace access.
- SDK flows for named agent discovery, task preparation, one-shot runs,
  query helpers, long-lived sessions, workspace overrides, shared state,
  runtime hooks, event listeners, streaming callbacks, cancellation, steering,
  follow-up prompts, and session reuse across turns.
- Runtime-backed sub-agents with synchronous or background execution,
  batched task submission, status snapshots, steering, continuation of
  completed sessions, duplicate-running-task protection, and inherited stream
  callbacks.
- Skill discovery, frontmatter parsing, metadata normalization, validation,
  `<available_skills>` prompt rendering with budget limits, activation state,
  and activation history.
- Checked examples covering SDK/session APIs, runtime hooks, custom tools,
  sub-agent pipelines, skills, streaming, cancellation, state stores, execution
  backends, workspace backends, and temporary tool injection.
- Tests covering public API construction, CLI task preparation, SDK resources,
  runtime cycles, tool planning, model-visible schema quality, workspace tools,
  vv-llm integration, and live DeepSeek smoke coverage.

Provider request serialization is intentionally delegated to the crates.io
`vv-llm` crate. Request-side provider behavior should be added there, while
this repository focuses on the agent runtime, tool system, SDK, prompts,
memory, and workspace execution layers.
