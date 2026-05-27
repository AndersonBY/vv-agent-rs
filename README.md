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
      background_sessions.rs
      config.rs
      constants.rs
      integrations.rs
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
        sub_agents.rs
        sub_agent_sessions.rs
        sub_task_manager.rs
        token_usage.rs
      sdk/
        client.rs
        mod.rs
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
      sub_agent_sessions.rs
      sub_task_manager.rs
      processes.rs
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
          workspace.rs
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
- Top-level exports for Python-style tool dispatch helpers, including
  `dispatch_tool_call` and `ToolNotFoundError`, in addition to `ToolRegistry`
  and `build_default_registry`.
- A CLI target inside the same package.
- Top-level modules aligned with the Python package: `background_sessions`,
  `cli`, `config`, `constants`, `integrations`, `llm`, `memory`, `processes`,
  `prompt`, `runtime`, `sdk`, `skills`, `tools`, `types`, and `workspace`.
- The `integrations::SkillIntegration` public trait mirrors the Python
  protocol with an `enabled()` capability check, and
  `integrations::protocols::SkillIntegration` is re-exported to match Python's
  `vv_agent.integrations.protocols` import path.
- The `constants` module exposes Python-style tool names, `WORKSPACE_TOOLS`,
  default tool schemas, workspace tool schemas, and convenience accessors for
  the `task_finish`, `ask_user`, and `activate_skill` schemas. It also exposes
  Python-matched `constants::tool_names` and `constants::workspace` submodules.
- `vv-llm = "0.1.0"` backed chat client construction through
  `build_vv_llm_from_local_settings`, settings-based endpoint resolution, and
  provider HTTP/protocol handling delegated to `vv-llm`, while keeping
  `ScriptedLlmClient` for deterministic tests. The scripted client now mirrors
  Python `ScriptedLLM` by accepting both fixed `LLMResponse` steps and callback
  steps that inspect the live `LlmRequest`, and it reports exhausted scripts
  explicitly with `LlmError::ScriptExhausted`. Resolved model metadata keeps
  Python-style ordered `endpoint_options` for all enabled endpoint bindings,
  the client builder constructs a vv-llm chat client for each enabled endpoint,
  uses Python-style randomized endpoint ordering by default (with an opt-out for
  deterministic ordering), retries each endpoint before failover, and prefers
  the last successful endpoint on subsequent calls, responses record
  `used_endpoint_id` / `used_model_id` / `stream_mode`, resolved metadata carries vv-llm
  `context_length` / `max_output_tokens`, and `build_vv_llm_settings` exposes
  the normalized `vv_llm::LlmSettings` used by the client builder.
- Split `llm/` modules matching Python's base/scripted/vv_llm_client layers,
  with the public `LlmClient` trait, scripted test client, and `vv-llm` backed
  production client kept behind stable top-level exports. Python-style public
  aliases such as `LLMClient`, `ScriptedLLM`, `ScriptStep`, and `VVLlmClient`
  are also exported for callers porting code from `v-agent`. `LlmClient` also
  exposes debug-dump configuration hooks, so SDK callers that inject a custom
  `llm_builder` can receive `AgentSDKOptions.debug_dump_dir` instead of
  bypassing Python-style request dump behavior.
- Split `memory/` submodules are public on the same import paths as Python,
  including `errors`, `manager`, `message_sanitizer`, `microcompact`,
  `post_compact_restore`, `session_memory`, and `token_utils`.
- `llm::apply_claude_prompt_cache` mirrors Python's Anthropic prompt-cache
  planning helper for Claude direct and Vertex requests, including stable
  system sections, tool-schema breakpoints, history breakpoints, thinking-block
  skipping, and the `anthropic_prompt_cache_enabled` opt-out metadata. Actual
  provider request serialization remains delegated to `vv-llm`; the helper is
  exposed for callers and future typed `vv-llm` cache-control support instead
  of reintroducing hand-written provider HTTP conversion here.
- LLM settings normalization keeps Python compatibility for `providers` /
  `backends`, default `VERSION`, endpoint API-key suffix extraction, and
  opt-in base64 key decoding before constructing `vv-llm` clients.
- Python `.py` settings files are supported for literal config templates:
  `LLM_SETTINGS = {...}` and `settings: SettingsDict = {...}` are parsed with
  Python-style booleans/nulls, comments, and trailing commas before resolution
  is delegated to `vv-llm`.
- A basic multi-cycle runtime that sends tool schemas to the LLM, executes tool
  calls, and converges on `task_finish` or `ask_user`.
- Split `runtime/` modules for background sessions, captured processes,
  cancellation, hooks, shell resolution, state
  stores, `engine.rs` main runtime execution, cycle-runner retry helpers,
  tool-call running helpers, tool-result extraction, sub-agent execution,
  sub-agent session registry, sub-task manager, and Python-style
  `runtime/backends/` submodules for base/inline/thread/celery/celery_tasks
  paths so deeper Python parity work can stay localized. `CycleRunner` and
  `ToolCallRunner` are now public runtime helpers as in Python, so embedders can
  run one LLM planning cycle or one tool-call batch without going through the
  full `AgentRuntime`; `runtime::{Checkpoint, InMemoryStateStore, StateStore}`
  and `runtime::engine` sub-agent session helpers also match Python's direct
  runtime import paths. `MAX_PTL_RETRIES` is available as the Python-style
  prompt-too-long retry constant alias.
- Runtime hooks modeled after Python `RuntimeHookManager`: callers can patch
  messages before memory compaction, patch LLM request messages/schemas, patch
  LLM responses, patch or short-circuit tool calls, and patch tool results.
  `RuntimeHookManager::has_hooks()` is available as a Python-style convenience
  alias for embedders.
- Runtime assistant messages preserve provider `raw.reasoning_content` in the
  transcript, matching Python `CycleRunner` behavior for reasoning-chain and
  resume flows.
- Runtime lifecycle logging through `log_handler`, with Python-style events for
  run start, cycle start, LLM response, tool result, completion, wait-user, and
  max-cycle exits, including configurable assistant/content/final-answer
  preview fields.
- Direct `AgentRuntime` usage now also resolves model context and output-token
  limits from the configured vv-llm `settings_file` plus `default_backend`
  before building memory thresholds; explicit task metadata still takes
  precedence.
- The in-crate `vv-agent` CLI mirrors Python `cli.py` flags for prompt,
  backend/model, settings file, workspace, max cycles, language, agent type,
  verbose logs, prompt bundle construction, JSON result payloads, and resolved
  vv-llm token-limit propagation into runtime memory metadata.
- Python-style runtime token usage helpers normalize raw provider usage payloads
  across prompt/completion and input/output naming variants, preserve the raw
  usage payload, and summarize per-cycle totals.
- The `vv-llm` backed client estimates prompt/completion usage when a provider
  response omits usage, preserving Python's fallback behavior for runtime
  accounting and memory-compaction heuristics. It also auto-enables vv-llm
  streaming for Python-matched reasoning/tool-call model families such as
  DeepSeek v4, Claude, Gemini, Kimi, Qwen3, GLM, GPT-5, and MiniMax. The same
  client now applies vv-llm-supported Python request options for DeepSeek
  reasoning temperature, Claude thinking model normalization and token budget,
  Gemini 3 preview routing, Qwen/GLM `-thinking` suffix routing, GPT/O-series
  `-high` alias routing, MiniMax multi-system message preparation, Python-style
  streaming `raw_content` block aggregation, and provider tool-call id/name
  normalization. `VvLlmClient` and SDK-built vv-llm runtimes also support
  Python-style debug request dumps via `debug_dump_dir`.
- Core runtime types expose Python-style `to_dict` / `from_dict` helpers for
  task, result, message, cycle, tool-call, and tool-result payloads, including
  legacy tool `status` plus `status_code` for worker interoperability. Agent
  result payloads round-trip aggregate and per-cycle token usage as structured
  data. Message dict payloads now use Python/OpenAI-style assistant tool-call
  shapes and can restore those payloads from Python checkpoints while
  preserving provider `extra_content`. `Message::to_openai_message` also
  mirrors Python multimodal/tool-call payload shaping, including assistant tool
  calls with `content: null`, optional reasoning content, provider
  `extra_content`, and user image blocks.
- `CeleryBackend` now supports a Python-style distributed execution path through
  a pluggable `CycleTaskDispatcher` and shared `StateStore`: it writes the
  initial checkpoint, dispatches one cycle at a time, returns worker terminal
  results, preserves checkpointed state on errors/max-cycles, and cleans up
  after the run. `RuntimeRecipe` also exposes Python-style dict helpers and a
  default SQLite checkpoint path under `<workspace>/.vv-agent-state`; the
  reusable `run_checkpointed_cycle` helper mirrors the worker-side
  `celery_tasks.run_single_cycle` checkpoint load / one-cycle execute / save or
  terminal cleanup flow.
- `AgentRuntime` now owns a configurable `RuntimeExecutionBackend` and delegates
  its cycle loop through `InlineBackend`, `ThreadBackend`, or `CeleryBackend`,
  matching Python `AgentRuntime.run -> execution_backend.execute(...)`
  semantics. Runtime cycle indexes are Python-compatible and start at `1`.
- Split `memory/` modules with Python-style compaction thresholds, local
  structured summaries, and runtime autocompaction before large follow-up LLM
  cycles. Runtime memory decisions can reuse provider prompt token totals from
  the previous cycle and add recent tool-result token estimates, matching
  Python `CycleRunner` / `MemoryManager` compaction heuristics. Optional
  Python-style memory warnings can append localized user guidance before
  compaction when usage crosses `memory_threshold_percentage`. Memory summaries
  also support Python-style `summary_callback(prompt, backend, model)`; runtime
  builds that callback from the configured `LlmClient`, so vv-llm backed clients
  can generate remote summaries without custom provider adapters, while callback
  failures fall back to local summaries.
- Large historical tool results can be persisted under `.memory/tool_results`
  and replaced with compact retrieval hints, matching Python `v-agent` artifact
  compaction behavior. Memory compaction now tries this artifact-only reduction
  before full summary and recomputes the prompt length without stale provider
  totals. Previously processed image payloads are also compacted by dropping
  historical `image_url` data once a later assistant message has consumed them.
  Repeated compaction preserves `original_user_messages` from earlier
  compressed memory blocks, so long sessions keep the user's initial request
  across multiple summary passes.
- Persistent session memory modeled after Python `SessionMemory`: durable
  entries are normalized, deduplicated, budget-pruned, optionally persisted
  under `.memory/session`, and injected back into runtime LLM requests as a
  `<Session Memory>` system context before and after compaction. The default runtime can
  use the configured `LlmClient` as the extraction callback, so vv-llm-backed
  clients handle session-memory extraction without custom provider adapters.
  Main tasks enable session memory by default like Python, while generated
  sub-tasks explicitly opt out unless overridden. Memory-summary backend/model
  selection follows Python priority: task metadata, local settings defaults,
  then runtime fallback backend and task model. Extraction callback failures are
  contained like Python, so a failed memory extraction leaves state unchanged
  instead of aborting the run.
- Python-style microcompact support clears old, large, compactable tool results
  before full summary compaction, preserving recent tool context while reducing
  prompt pressure during long runs. Task metadata can override the compactable
  tool allowlist with `microcompact_compactable_tools`.
- Prompt-too-long retries modeled after Python `CycleRunner`: runtime detects
  common provider context-window errors, forces normal memory compaction once,
  then falls back to emergency compaction slices that preserve system and recent
  tool context before retrying. If all PTL retries are exhausted, runtime now
  returns a Python-style `CompactionExhaustedError` with the attempt count and
  last provider error.
- Post-compaction file context restore modeled after Python
  `post_compact_restore`: summaries now track file actions as structured
  `path/action/summary` entries and restore key workspace files under a bounded
  `<Post-Compaction File Context>` block.
- Python-style message sanitization for resume/compaction: blank and
  thinking-only assistant messages are removed, orphan tool results and
  unresolved tail tool calls are pruned, and memory compaction normalizes stale
  tool-call boundaries before summarizing.
- Split `prompt/` modules modeled after Python `vv_agent.prompt`: system prompt
  builder sections, stable prompt hashes, raw section metadata, localized tool
  templates, available skills rendering, and prompt-cache break tracking.
- Split `tools/` modules modeled after Python `v-agent`: `base`, `builtins`,
  `registry`, dispatcher, canonical `schemas/` domain modules, shared `common`
  helpers, and focused handler modules. `tools::handlers::common` mirrors the
  Python handler helper import path for JSON rendering, TODO-list normalization,
  and workspace path resolution. `tools::builtins` exposes the Python-matched
  `build_default_registry` import path, while `ToolRegistry` supports
  Python-style custom tool registration with default empty parameters or
  explicit JSON Schema parameters. `tools::handlers` now re-exports the same
  direct handler function names as Python `vv_agent.tools.handlers.__all__`,
  and each focused handler module exposes its Python-matched entrypoint.
- Python-style tool dispatch normalizes raw LLM tool arguments into structured
  error tool results, fills missing / pending tool call ids, maps wait-user
  directives to `WAIT_RESPONSE`, and returns `tool_not_found` without dropping
  the tool result from the transcript.
- Split public `skills/` modules for Python-style skill models, directory
  discovery, frontmatter parsing, metadata normalization, validation modes,
  diagnostics, and `<available_skills>` prompt rendering with the same
  progressive budget degradation used by `v-agent`.
- Split `sdk/` modules matching Python's `types`, `resources`, `session`, and
  `client` layers while keeping the crate-level SDK exports stable.
  `sdk::LLMBuilder` and `sdk::RuntimeLogHandler` are exposed as
  Python-matched aliases for ported callers.
- `activate_skill` now reuses the public skill parser / normalization layer and
  keeps handler-local state helpers for activation tracking.
- Default tool schemas now use reference-quality descriptions derived from
  Python `v-agent`, with extra actionable guidance for high-impact tools such
  as `task_finish`, `list_files`, `write_file`, `file_str_replace`,
  `file_info`, `compress_memory`, `check_background_command`, and `read_image`,
  so the model sees complete operational guidance for file access, grep,
  bash/background commands, todos, skills, images, and sub-agents.
- Planned tool schemas include Python-style dynamic runtime hints for shell
  execution, so `bash` advertises the actual shell prefix or invalid shell
  configuration in the LLM-visible description. Runtime runs freeze that hint
  into task metadata before backend dispatch so distributed workers and later
  cycles use the same shell guidance. The public
  `runtime::tool_planner::{plan_tool_names, plan_tool_schemas}` functions now
  mirror Python's `runtime.tool_planner` module, while `ToolRegistry` keeps thin
  compatibility wrappers. Tool planning also keeps Python-style
  `extra_tool_names` in the planned name list and includes `todo_write` in the
  default workspace tool set before schema filtering.
- Shell resolution now lives in `runtime::shell`, matching Python's
  `runtime/shell.py` split, including the public `build_shell_invocation`
  helper. Bash execution and tool-planner runtime hints share the same
  resolver, so configured shells, `bash_env` environment overrides, and
  auto-confirm behavior do not diverge. Bash process environment construction
  also mirrors Python's Windows defaults for `PYTHONUTF8` and
  `PYTHONIOENCODING` while preserving explicit overrides.
- Built-in control tools (`task_finish`, `ask_user`, `todo_write`), with
  Python-style TODO validation, generated ids, status/priority defaults, and
  timestamp preservation; core workspace tools (`list_files`, `file_info`,
  `read_file`, `write_file`, `file_str_replace`, `workspace_grep`,
  `read_image`, with image-message injection limited to `native_multimodal`
  tasks, Python-style `read_file` numeric-string line range parsing,
  `list_files` numeric-string limits, Python-style scalar text coercion for
  `write_file`/`file_str_replace`, `file_str_replace` numeric-string
  replacement limits, hidden-file filtering, Python-style local ripgrep fast
  path, and scan-limit estimate payloads, and
  `workspace_grep` regex search, numeric-string limits/context parsing, text
  content with structured matches kept in metadata, Python-style glob filtering,
  Python-style scalar text coercion, configured workspace backend support,
  local ripgrep JSON fast path with fallback, Python-style text truncation, and
  Python-style structured payload limits, plus
  single-file grep targets that bypass hidden/ignored directory filtering like
  Python);
  memory notes through `compress_memory`; and `bash` /
  `check_background_command` command tools with captured output, Python-style
  replacement decoding, stdin, numeric-string timeout parsing,
  metadata-controlled shell selection via `bash_shell`, foreground timeout
  handoff, background polling, and automatic terminal background-session
  listener notifications. `BackgroundSessionManager::start` also mirrors
  Python's manager-level command startup path, including shell preparation,
  stdin, auto-confirm, and process environment overrides. Background listener
  adoption supports explicit `started_at` timestamps, and listener failures are
  isolated so one listener cannot prevent the remaining subscribers from
  receiving terminal events.
- Python-compatible workspace path safety: `LocalWorkspaceBackend` rejects
  paths outside the workspace by default, expands `~/...` paths before applying
  the same safety checks, and file/image/grep/bash tools keep
  metadata-controlled outside-path access for trusted tasks. Tool contexts merge
  `ExecutionContext.metadata` with task metadata, with task metadata taking
  precedence, matching Python runtime integration behavior.
- Split `workspace/` modules matching Python's base/local/memory/s3 layers while
  keeping `FileInfo`, `WorkspaceBackend`, and concrete backends exported from
  the crate root.
- Python-style workspace backends: `LocalWorkspaceBackend` and
  `MemoryWorkspaceBackend` honor base-relative `**` glob matching, return
  deterministic POSIX-style paths, preserve memory directories, and report
  missing in-memory files with `NotFound` errors. `S3WorkspaceBackend` is backed
  by the Rust `object_store` S3 client for S3-compatible buckets, supports
  workspace prefixes, append, glob listing, metadata lookup, and Python-style
  dotted suffixes. Workspace backend types are exported from the top-level crate
  API.
- Python-compatible `read_file` response limiting: large reads return file
  statistics, request size, limits, and a suggested line range instead of
  flooding the LLM context.
- Python-compatible directive handling inside a tool-call batch: when a tool
  asks for user input or finishes the task, later tool calls in the same LLM
  response are recorded as skipped results instead of disappearing from the
  transcript.
- Python-style runtime cancellation controls: cloneable `CancellationToken`
  values support idempotent cancellation, callback registration, parent/child
  propagation, and `RuntimeRunControls` cancellation checks before cycles and
  between tool calls, returning a failed result with a `run_cancelled` event.
- `RuntimeRunControls` also supports Python-style before-cycle message
  providers and interruption message providers. Callers can inject messages at
  the start of each cycle before compaction / LLM planning, or interrupt a
  tool-call batch after a completed tool so later calls are marked
  `skipped_due_to_steering` and the queued message is carried into the next
  cycle.
- Python-style `ExecutionContext` is available for runtime integrations, with
  cancellation token, stream callback, state store, and metadata fields. Runtime
  cancellation checks now honor tokens supplied through the context as well as
  direct `RuntimeRunControls`; context metadata is passed into tool execution;
  stream callbacks are forwarded into `vv-llm` streaming completions and can
  also be configured through `AgentSDKOptions`.
- Python-inspired runtime backend helpers: `InlineBackend`, `ThreadBackend`,
  `CeleryBackend`, and serializable `RuntimeRecipe` mirror the Python backend
  API surface for ordered `parallel_map`, thread `submit`, inline Celery
  fallback, distributed runtime recipe data, and `execute` cycle loops with
  cancellation and max-cycle results. `AgentRuntime` delegates through the same
  backend abstraction instead of running a separate internal loop, and passes
  that backend into tool context so synchronous batch sub-tasks can use
  `execution_backend.parallel_map` like Python `v-agent`.
- Runtime checkpoint stores modeled after Python `runtime.state` and
  `runtime.stores.sqlite`: `Checkpoint`, `InMemoryStateStore`, and
  `SqliteStateStore` persist messages, cycles, status, and shared state for
  distributed / resumable execution plumbing. `RedisStateStore` also mirrors
  Python's `vv_agent:checkpoint:{task_id}` key layout for Celery-adjacent
  checkpoint persistence.
- SDK sessions expose Python-style cancellation through `cancel()` and a
  cloneable `SessionCancellationHandle`; active session runs receive the
  cancellation token, queued steering/follow-up prompts are cleared, and
  listeners see `session_cancel_requested`.
- Runtime-backed sub-agent support for `create_sub_task` / `sub_task_status`:
  configured `AgentTask.sub_agents` can run synchronously or via async
  `wait_for_completion=false`, with batch aggregation and status/snapshot
  polling. `create_sub_task` also accepts Python-style boolean coercion for
  `include_main_summary` and `wait_for_completion`, so common string values
  such as `"true"` and `"0"` behave the same as in Python. `SubTaskRequest::new`
  uses the same defaults as Python's dataclass, and `SubTaskOutcome::to_dict`
  emits the Python payload shape for callers that serialize sub-agent results.
  A Python-style active sub-agent session registry exposes
  `get_sub_agent_session`, `subscribe_sub_agent_session`, and
  `steer_sub_agent_session`, and `sub_task_status(message=...)` can queue
  steering messages for sessions registered during active runs or continue
  completed sessions attached to `SubTaskManager`, including Python-style
  `wait_for_response` coercion and max-cycle continuation rejection. The
  Python-private `_register_sub_agent_session` / `_unregister_sub_agent_session`
  guarded aliases are also exposed for `runtime::engine` parity.
  `SubTaskManager::attach_session` also
  tracks Python-style session event snapshots with recent activity, latest
  cycle/tool-call metadata, and visible workspace file listings. Runtime-backed
  sub-tasks are now session-driven and temporarily registered only while a run
  is active, so completed async sub-tasks can be continued through
  `sub_task_status(message=...)` while preserving prior messages and shared
  state without leaking stale global sessions. `SubTaskManager::submit` rejects
  duplicate running `task_id` submissions like Python instead of overwriting the
  active record. Before a completed session is resumed, stale resume messages
  are sanitized the same way as Python: empty / thinking-only assistant messages
  and unresolved tail tool calls are removed before the continuation prompt is
  appended. Attached runtime-backed sub-agent sessions also retain Python's
  resolved backend/model payload across follow-up continuations, so
  `sub_task_status` keeps reporting the model used even when the continuation
  outcome does not repeat that metadata. For embedders that need Python-style
  direct manager inspection, `SubTaskManager::get` and `wait_for_record` return
  a read-only `ManagedSubTaskSnapshot` instead of exposing thread handles.
- Sub-agent model/backend resolution follows Python safety rules: a different
  sub-agent model requires a runtime `settings_file`, otherwise the sub-task
  fails explicitly instead of silently reusing the parent LLM client. When
  settings are configured, resolution is delegated through the same `vv-llm`
  settings builder used by top-level clients.
- Generated sub-agent prompts now use the Python-style prompt builder options
  inherited from the parent task, and store `system_prompt_sections` in task
  metadata for prompt-cache and downstream context parity.
- Python-style `activate_skill` behavior for allowed skills: inline skill
  entries and `SKILL.md` locations load instructions, update `active_skills`,
  and record activation history.
- Python-style SDK session continuation basics: `AgentSession::follow_up`
  queues automatic completed-run follow-ups, `steer` has priority for
  `continue_run(None)`, `clear_queues` drops pending prompts, and `query`
  returns the final answer or a status-specific error such as
  `status=wait_user`. Sessions also support listener registration for queue and
  run lifecycle events such as `session_run_start`, `session_run_end`,
  `session_follow_up_queued`, and `session_steer_queued`, and `AgentRun::to_dict`
  includes Python-style status details, todo list, resolved model metadata, and
  structured aggregate/per-cycle token usage. Runtime-backed
  sessions forward runtime events such as `tool_result` to session listeners;
  a cloneable `SessionSteeringHandle` lets those listeners queue steering while
  a run is active, which injects the prompt before the next cycle or interrupts
  the current tool batch with `skipped_due_to_steering`, `session_steer_interrupt`,
  and `run_steered` events. Sessions also subscribe to running background
  commands reported by `bash` / `check_background_command`; terminal background
  events emit `background_command_completed` / `background_command_terminal`
  and queue a system notification as steering while the run is active. Background
  session snapshots keep Python's stable `shell` field shape, including
  `null` when no shell was recorded.
  Runtime-backed sessions preserve prior messages and shared state across
  prompts, so follow-up turns see the same conversation and TODO/memory state
  instead of starting from a fresh task. SDK-created runtime sessions also
  inherit `AgentSDKOptions.workspace` for both session state and tool execution
  context, and `AgentSDKOptions.log_preview_chars` for forwarded runtime event
  previews. SDK startup shell defaults (`bash_shell`,
  `windows_shell_priority`, and `bash_env`) are merged into agent task metadata
  like Python, with agent-level environment values taking precedence. SDK
  sessions now also carry a stable `session_id` into every task's metadata;
  callers can use `AgentSDKClient::create_session_with_id` or
  `create_agent_session_with_id` when they need a deterministic session id.
  Generated session ids now use the same 12-character hex shape as Python, and
  session constructors/helpers accept initial shared state while still adding
  Python's default `todo_list: []`.
  Session continuation is covered for Python's multi-tool wait-user case: a
  first `ask_user` pauses the run, later tool calls in the same batch are
  recorded as skipped, and `continue_run(Some(...))` resumes the same
  conversation to completion.
  Session workspace overrides are supported through
  `AgentSDKClient::create_session_with_workspace`; the override updates session
  state, runtime workspace metadata, and the file-tool workspace backend.
  `AgentSession` also exposes Python-style read accessors for `agent_name`,
  `definition`, `workspace`, `messages`, `shared_state`, `latest_run`, and
  `running`.
  One-shot SDK runs can also use per-call workspace overrides through
  `run_with_agent_in_workspace`, `run_agent_in_workspace`, or `run_in_workspace`,
  matching Python `run(..., workspace=...)` behavior.
  Sessions also reuse one `SubTaskManager` across turns, so a later prompt can
  inspect or continue async sub-tasks created by an earlier prompt in the same
  session.
- `AgentSDKClient::query` mirrors Python client query semantics: it returns the
  final answer for completed runs and reports non-completed statuses with
  snake_case status values such as `status=wait_user`. Named-agent query
  compatibility wrappers are available through `query_agent`,
  `query_agent_with_require_completed`, and the workspace-specific query
  helpers.
- `AgentSDKClient` auto-discovers named profiles from `.vv-agent/agents.json`,
  exposes `list_agents`, and can run a profile by name through `run_agent`,
  preserving the profile name in `AgentRun.agent_name`. Plain `run()` follows
  Python selection semantics: use the default agent, auto-select a single
  profile, or return a clear error when no profile or multiple profiles are
  configured.
- Runtime-backed sub-agent sessions inherit the parent run's LLM stream
  callback, so streamed provider events continue to flow through nested agent
  execution like Python. Child stream events are enriched with `task_id`,
  `session_id`, and `sub_agent_name`, and parent log/event handlers receive
  matching `sub_agent_*` events.
- The `runtime` module now exposes Python-style public names including
  `InlineBackend`, `CancelledError`, and `ManagedSubTask` alongside the existing
  hook, state-store, cancellation, cycle-runner, and tool-runner types.
- Memory token utilities now prefer `vv-llm::utilities::count_tokens` for
  supported tokenizers and fall back to the Python-style CJK-aware estimator for
  unsupported models. They also accept structured JSON payloads by serializing
  them before estimation, and message-level fallback uses
  `Message::to_openai_message(true)` so multimodal user image blocks match
  Python's OpenAI-compatible payload shape. The module also exposes
  settings-backed `resolve_model_token_limits` /
  `resolve_model_token_limits_from_file` helpers that read model context and
  output budgets through the crates.io `vv-llm` settings model.
  `memory::COMPACTABLE_TOOLS` exposes the same default microcompact tool
  allowlist as Python.
- SDK one-shot runs no longer require a prebuilt runtime: by default the client
  builds a `vv-llm` backed runtime from `AgentSDKOptions.settings_file`, while
  tests and embedders can inject an `LlmBuilder` for deterministic clients.
  `AgentSDKOptions.runtime_hooks`, `log_handler`, `tool_registry_factory`,
  `execution_backend`, `debug_dump_dir`, and custom `resource_loader` are
  applied to SDK flows, matching Python's SDK extension points. SDK-built
  runtimes apply resolved vv-llm token limits as `model_context_window` and
  `reserved_output_tokens` metadata unless the caller already supplied those
  keys. Module-level
  one-shot helpers `run_with_options_and_agent` and
  `query_with_options_and_agent` mirror Python `sdk.run(...)` / `sdk.query(...)`
  while keeping Rust signatures explicit. `AgentSDKClient::run_agent_with_request`
  and `run_with_agent_request` expose the same one-shot request path used by
  sessions, so callers can pass shared state, initial messages, cancellation,
  steering, and per-run metadata without constructing a long-lived session.
- SDK task preparation now builds Python-style prompt bundles from
  `AgentDefinition.description` when no raw `system_prompt` is provided,
  preserving generated `system_prompt_sections` metadata for cache and
  debugging flows. `prepare_task_for_agent` exposes this path for named
  profiles, and `system_prompt_template` is treated like Python: it replaces the
  agent definition text while still going through the full prompt builder.
  Relative `skill_directories` are resolved from the SDK workspace during task
  preparation and one-shot runs, matching Python's available-skills prompt
  behavior. Runtime limit fields are clamped during SDK task preparation so
  invalid `max_cycles`, memory compaction threshold, or memory threshold
  percentage values fall back to Python-compatible safe ranges. SDK prepare,
  one-shot run, and session flows now generate Python-style unique task ids
  (`agentName_<8 hex>`), avoiding checkpoint and session-memory scope
  collisions across repeated SDK runs. `AgentSDKClient::new_with_agent`,
  `new_with_agents`, `prepare_task`, and `prepare_task_in_workspace` now cover
  Python's default/only-agent task preparation path while retaining explicit
  Rust alternatives. SDK sessions use the same effective agent definition as
  one-shot runs, so startup shell defaults, bash environment overrides, prompt
  templates, and discovered skill directories also apply to session prompts.
- SDK clients can now create sessions from the configured default agent or the
  only registered profile through `create_default_session*` helpers, and can
  create sessions by profile name without manually passing the copied
  `AgentDefinition`, matching Python's `client.create_session(...)` selection
  behavior while using explicit Rust method names.
- Python-style tool planning from `AgentTask` flags, plus `.vv-agent`
  discovery for `agents.json`, prompt templates, and skill directories.
  `agents.json` now carries full agent fields including sub-agent definitions,
  tool flags, shell defaults, metadata, and resource paths. Resource paths
  expand `~` like Python, and `AgentResourceLoader::discover_force_reload`
  refreshes cached resources after on-disk changes. SDK clients can also inject
  a custom `AgentResourceLoader` to discover agents and prompt templates from
  non-default roots. Python hook files under `.vv-agent/hooks` are discovered
  on the Python-style `DiscoveredResources.hooks` field, with `hook_files`
  retained as a Rust compatibility alias, and reported through diagnostics;
  Rust hook execution uses `AgentSDKOptions.runtime_hooks`.
- SDK client, tool registry, workspace backends, and shared protocol types.
- Smoke tests covering public API construction, Rust SDK usage, vv-llm
  integration, runtime tool cycles, schema parity, and workspace tools.

Deeper parity work against the Python implementation is still pending for
production distributed-worker integrations and provider-specific request
serialization edge cases. The migration target is to copy Python `v-agent`
capabilities, implementation shape, and behavior as completely as possible, not
merely provide a minimal Rust wrapper.
