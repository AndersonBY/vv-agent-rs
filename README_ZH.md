# vv-agent-rs

[English](README.md)

VectorVein Agent 库的 Rust 工作空间。这个 crate 尽量贴近 Python `v-agent/src/vv_agent` 的公开表面，让 Rust 调用方先能依赖稳定的顶层 API，而更深层的运行时一致性则按模块逐步补齐。

## 目录结构

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

包名是 `vv-agent`；库目标以 `vv_agent` 导入，符合连字符包名的 Rust 命名规则。

## 验证

在 `vv-agent-rs/` 目录下运行：

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

真实 DeepSeek smoke test 默认关闭，会使用本地 vv-llm 开发配置文件，且不会打印凭据：

```bash
VV_AGENT_RUN_LIVE_TESTS=1 cargo test --test live_deepseek -- --ignored
```

## 当前范围

当前 Rust 实现包括：

- 一个可用的 Cargo workspace，包含主 `vv-agent` 包。
- 一个库目标，暴露与 Python `vv_agent.__init__` 类似的顶层 API 类型和函数。
- crate 顶层也导出了 Python 风格工具调度入口，包括 `dispatch_tool_call` 和 `ToolNotFoundError`，并继续导出 `ToolRegistry` 与 `build_default_registry`。
- 同一 package 内的 CLI 目标。
- 与 Python 包对齐的顶层模块：`background_sessions`、`cli`、`config`、`constants`、`integrations`、`llm`、`memory`、`processes`、`prompt`、`runtime`、`sdk`、`skills`、`tools`、`types` 和 `workspace`。
- `integrations::SkillIntegration` 公开 trait 已对齐 Python protocol，使用 `enabled()` 作为能力开关；同时通过 `integrations::protocols::SkillIntegration` 补齐 Python `vv_agent.integrations.protocols` 导入路径。
- `constants` 模块已暴露 Python 风格工具名、`WORKSPACE_TOOLS`、默认工具 schema、workspace 工具 schema，以及 `task_finish`、`ask_user`、`activate_skill` schema 的便捷访问函数；同时补齐 Python 对齐的 `constants::tool_names` 和 `constants::workspace` 子模块路径。
- 基于 crates.io 官方 `vv-llm = "0.1.0"` 的 chat client 构建，通过 `build_vv_llm_from_local_settings` 解析配置化 endpoint，并把 provider HTTP / 协议处理交给 `vv-llm`；resolved model metadata 会保留 Python 风格的有序 `endpoint_options`，覆盖所有启用的 endpoint binding，client builder 会为每个启用 endpoint 构造 vv-llm chat client，默认使用 Python 风格随机化 endpoint 顺序（也可显式关闭以获得确定性顺序），先按 endpoint 内 retry，再 failover，并在后续请求优先复用上次成功 endpoint，响应 raw 也会记录 `used_endpoint_id` / `used_model_id` / `stream_mode`；resolved metadata 会携带 vv-llm 解析出的 `context_length` / `max_output_tokens`；`build_vv_llm_settings` 也会暴露 client builder 使用的归一化 `vv_llm::LlmSettings`；同时保留 `ScriptedLlmClient` 用于确定性测试，并已对齐 Python `ScriptedLLM` 的固定 `LLMResponse` step 与可检查当前 `LlmRequest` 的 callback step；脚本队列耗尽时会显式返回 `LlmError::ScriptExhausted`。`llm/` 也已按 Python 的 base / scripted / vv_llm_client 层级拆分，同时保持 crate 顶层稳定导出，并提供 `LLMClient`、`ScriptedLLM`、`ScriptStep`、`VVLlmClient` 等 Python 风格公开别名，方便从 `v-agent` 迁移调用代码；`LlmClient` 还暴露 debug dump 配置扩展点，因此 SDK 调用方即使注入自定义 `llm_builder`，也能收到 `AgentSDKOptions.debug_dump_dir`，不再绕过 Python 风格请求落盘行为。
- `llm::apply_claude_prompt_cache` 已对齐 Python 的 Anthropic prompt-cache 规划 helper，覆盖 Claude direct / Vertex 请求、稳定 system sections、tool schema breakpoint、history breakpoint、跳过 thinking block，以及 `anthropic_prompt_cache_enabled` metadata 关闭开关。真实 provider 请求序列化仍交给 `vv-llm`；这里先公开 helper，等待 `vv-llm` 后续 typed cache-control 字段支持，而不是在本仓库重新手写 provider HTTP 转换。
- `memory/` 的子模块也已按 Python 导入路径公开，包括 `errors`、`manager`、`message_sanitizer`、`microcompact`、`post_compact_restore`、`session_memory` 和 `token_utils`。
- LLM settings 归一化已兼容 Python 的 `providers` / `backends`、默认 `VERSION`、endpoint API key 后缀提取，以及可选 base64 key 解码，然后再构造 `vv-llm` client。
- 已支持 Python `.py` settings 字面量模板：`LLM_SETTINGS = {...}` 和 `settings: SettingsDict = {...}` 都会按 Python 风格解析布尔值、空值、注释和尾逗号，再交给 `vv-llm` 做 endpoint / backend 解析。
- 一个基础 multi-cycle runtime，可以把 tool schemas 发给 LLM、执行工具调用，并通过 `task_finish` 或 `ask_user` 收敛。
- `runtime/` 已拆成 background sessions、captured processes、cancellation、hooks、shell resolution、state stores、`engine.rs` 主运行时、cycle-runner retry helper、tool-call runner helper、工具结果解析、sub-agent 执行模块、sub-agent session registry、sub-task manager，以及 Python 风格 `runtime/backends/` 子模块（base / inline / thread / celery / celery_tasks），让后续继续补齐 Python parity 时改动更集中；crate 顶层仍保留薄 re-export wrapper 兼容既有调用方。`CycleRunner` 和 `ToolCallRunner` 也已像 Python 一样作为公开 runtime helper 导出，嵌入方可以只执行单轮 LLM planning 或单批工具调用，而不必走完整 `AgentRuntime`；`runtime::{Checkpoint, InMemoryStateStore, StateStore}` 和 `runtime::engine` 下的 sub-agent session helper 也对齐了 Python 的直接 runtime 导入路径；`MAX_PTL_RETRIES` 也作为 Python 风格 prompt-too-long retry 常量别名暴露。
- 已加入参考 Python `RuntimeHookManager` 的 runtime hooks：调用方可以在 memory compaction 前改写 messages，改写 LLM 请求的 messages / schemas、改写 LLM 响应、改写或短路工具调用，并改写工具结果。`RuntimeHookManager::has_hooks()` 也已作为 Python 风格便捷方法暴露。
- Runtime assistant message 现在会把 provider `raw.reasoning_content` 保留到 transcript，对齐 Python `CycleRunner` 的 reasoning-chain / resume 行为。
- `log_handler` 已可接收 Python 风格 runtime 生命周期事件，包括 run start、cycle start、LLM response、tool result、completed、wait-user 和 max-cycles，并提供可配置的 assistant / content / final-answer preview 字段。
- 直接使用 `AgentRuntime` 时，现在也会根据已配置的 vv-llm `settings_file` 和 `default_backend` 解析模型 `context_length` / `max_output_tokens`，再构建 memory threshold；显式 task metadata 仍然优先。
- 同一 package 内的 `vv-agent` CLI 已对齐 Python `cli.py` 的核心参数：prompt、backend/model、settings file、workspace、max cycles、language、agent type、verbose logs、prompt bundle 构建、JSON 结果输出，以及把 vv-llm resolved token limits 传入 runtime memory metadata。
- 已补 Python 风格 runtime token usage helper：可归一化 provider 原始 usage payload 中 prompt/completion 与 input/output 命名差异，保留 raw usage，并汇总逐 cycle token totals。`vv-llm` backed client 在 provider 未返回 usage 时也会估算 prompt/completion usage，避免 runtime 计费汇总和 memory compaction 启发式拿到全 0；同时也会像 Python 一样对 DeepSeek v4、Claude、Gemini、Kimi、Qwen3、GLM、GPT-5 和 MiniMax 等 reasoning / tool-call 模型自动启用 vv-llm streaming，并在 vv-llm 已支持的字段范围内补齐 DeepSeek reasoning temperature、Claude thinking 模型名 / token budget、Gemini 3 preview 路由、Qwen/GLM `-thinking` 后缀路由、GPT/O 系列 `-high` alias 路由、MiniMax 多 system 消息预处理、streaming `raw_content` block 聚合、provider tool-call id/name 归一化，以及 `debug_dump_dir` 请求消息调试落盘。
- 核心 runtime 类型已补 Python 风格 `to_dict` / `from_dict` helper，覆盖 task、result、message、cycle、tool-call 和 tool-result payload，并保留工具结果 legacy `status` + `status_code` 双字段以便 worker 互通。Agent result payload 也会完整 round-trip 聚合和逐 cycle 的 token usage 结构化数据。`Message::to_openai_message` 也已对齐 Python 的 multimodal / tool-call payload 形状，包括 assistant tool calls 的 `content: null`、可选 reasoning content、provider `extra_content` 和 user image blocks。
- `CeleryBackend` 已支持 Python 风格 distributed 执行路径：通过可插拔 `CycleTaskDispatcher` 和共享 `StateStore` 写入初始 checkpoint、逐 cycle 分发 worker、返回 worker terminal result、在错误 / max-cycles 时保留 checkpoint 状态，并在运行结束后清理 checkpoint。`RuntimeRecipe` 也补了 Python 风格 dict helper 和 `<workspace>/.vv-agent-state` 下的默认 SQLite checkpoint 路径；可复用的 `run_checkpointed_cycle` helper 对齐 worker 侧 `celery_tasks.run_single_cycle` 的 checkpoint load / 单 cycle execute / 保存或 terminal 清理流程。
- `AgentRuntime` 现在持有可配置的 `RuntimeExecutionBackend`，会把 cycle loop 委托给 `InlineBackend`、`ThreadBackend` 或 `CeleryBackend`，对齐 Python `AgentRuntime.run -> execution_backend.execute(...)` 语义；runtime cycle index 也改为与 Python 一致，从 `1` 开始。
- `memory/` 已拆成 manager、summary 和 token_utils，支持 Python 风格压缩阈值、本地结构化摘要，并在长上下文 follow-up cycle 前自动压缩。Runtime memory 决策也会复用上一轮 provider prompt token totals，并额外估算最近工具结果 token，对齐 Python `CycleRunner` / `MemoryManager` 的压缩启发式；也支持可选的 Python 风格 memory warning，在使用量超过 `memory_threshold_percentage` 时先追加本地化用户提示。Memory summary 也补齐 Python 风格 `summary_callback(prompt, backend, model)` 路径；runtime 会用已配置的 `LlmClient` 构造该 callback，因此 vv-llm-backed client 可以直接生成远端摘要，不需要额外 provider adapter，callback 失败时会回退到本地摘要。
- 历史大工具结果可以持久化到 `.memory/tool_results`，并在上下文里替换为带 retrieval hint 的压缩内容，对齐 Python `v-agent` 的 artifact compaction 行为。Memory compaction 会先尝试这种 artifact-only reduction，并在不使用过期 provider token totals 的情况下重新计数；若已经回到阈值内，就不再进入 full summary。已经被后续 assistant 消费过的历史图片消息也会丢弃 `image_url` payload，只保留压缩标记。重复 compaction 会继续保留旧压缩块里的 `original_user_messages`，避免长会话多次摘要后丢失用户最初请求。
- 已加入参考 Python `SessionMemory` 的持久化 session memory： durable entries 会归一化、去重、按预算裁剪，可保存到 `.memory/session`，并在 compaction 前后作为 `<Session Memory>` system context 注入后续 LLM 请求。默认 runtime 可以直接使用已配置的 `LlmClient` 作为 extraction callback，因此 vv-llm-backed client 不需要额外 provider adapter。主任务会像 Python 一样默认启用 session memory，自动生成的子任务默认显式关闭，除非 metadata 覆盖。Memory summary backend/model 选择也对齐 Python 优先级：task metadata、local settings defaults、runtime fallback backend 和 task model；extraction callback 失败会像 Python 一样被隔离，不会让本轮运行中断，也不会写入半更新状态。
- 已加入 Python 风格 microcompact：在 full summary compaction 之前清理旧的大型可压缩工具结果，保留近期工具上下文，同时降低长任务的 prompt 压力；task metadata 可以通过 `microcompact_compactable_tools` 覆盖可压缩工具 allowlist。
- 已加入参考 Python `CycleRunner` 的 prompt-too-long 重试：runtime 会识别常见 provider 上下文超限错误，先强制 normal memory compaction，再退到 emergency compaction 切片，并保留 system message 和近期工具上下文后重试。若所有 PTL 重试都耗尽，runtime 会返回 Python 风格 `CompactionExhaustedError`，包含 attempts 和最后一个 provider error。
- 已加入参考 Python `post_compact_restore` 的 compaction 后关键文件恢复：summary 会用结构化 `path/action/summary` 记录文件动作，并在预算内把关键 workspace 文件内容放回 `<Post-Compaction File Context>`。
- 已加入 Python 风格 resume / compaction message sanitizer：会移除空 assistant、thinking-only assistant、孤儿 tool result 和未完成的尾部 tool calls，并在 memory compaction 前归一化陈旧 tool-call 边界。
- `prompt/` 已按 Python `vv_agent.prompt` 拆分：支持 system prompt builder sections、stable prompt hash、raw section metadata、本地化工具模板、available skills 渲染和 prompt-cache break tracking。
- `tools/` 已按 Python `v-agent` 的结构拆分为 `base`、`builtins`、`registry`、dispatcher、canonical `schemas/` domain modules、共享 `common` helper 和各个 handler 模块；`tools::handlers::common` 对齐 Python handler helper 导入路径，可用于 JSON 渲染、TODO list 归一化和 workspace path 解析。`tools::builtins` 暴露 Python 对齐的 `build_default_registry` 导入路径，`ToolRegistry` 支持 Python 风格自定义工具注册，可使用默认空参数 schema 或显式 JSON Schema 参数。`tools::handlers` 现在按 Python `vv_agent.tools.handlers.__all__` 导出同名直接 handler 函数，各个 focused handler 模块也暴露 Python 对齐的入口函数。
- 已补 Python 风格 tool dispatch：会把 LLM 原始工具参数解析错误转换成结构化 tool result，补齐 missing / pending tool call id，把 wait-user directive 映射为 `WAIT_RESPONSE`，并在未知工具时返回 `tool_not_found`，避免 transcript 丢失工具结果。
- 公开 `skills/` 模块已拆成 Python 风格的 skill model、目录发现、frontmatter 解析、metadata 归一化、validation mode、diagnostics 和 `<available_skills>` prompt 渲染，并支持与 `v-agent` 一致的预算降级策略。
- `sdk/` 已按 Python 的 `types`、`resources`、`session` 和 `client` 层级拆分，同时保持 crate 顶层 SDK 导出稳定，并暴露 `sdk::LLMBuilder`、`sdk::RuntimeLogHandler` 这两个 Python 对齐的别名，方便迁移调用代码。
- `activate_skill` 现在复用公开 skill parser / normalization 层，handler 内只保留 activation state helper。
- 默认工具 schema 使用参考 Python `v-agent` 的高信息量描述，并对 `task_finish`、`list_files`、`write_file`、`file_str_replace`、`file_info`、`compress_memory`、`check_background_command`、`read_image` 等高影响工具补充更具体的操作约束，让模型拿到文件访问、grep、bash / 后台命令、todo、skills、图片和 sub-agent 的完整操作指引。
- planned tool schemas 已加入 Python 风格动态 runtime shell hint，`bash` 会在 LLM 可见 description 里提示实际 shell 前缀或 shell 配置错误；runtime 会在 backend dispatch 前把该 hint 冻结进 task metadata，确保分布式 worker 和后续 cycle 使用同一份 shell 指引。`runtime::tool_planner::{plan_tool_names, plan_tool_schemas}` 也已按 Python `runtime.tool_planner` 模块公开，`ToolRegistry` 只保留薄兼容 wrapper。工具规划也会像 Python 一样在 planned name 阶段保留 `extra_tool_names`，并把 `todo_write` 放进默认 workspace 工具集，schema 输出阶段再过滤未注册工具。
- shell 解析已抽到 `runtime::shell`，对齐 Python 的 `runtime/shell.py` 拆分，并补齐公开的 `build_shell_invocation` helper；`bash` 实际执行和 tool-planner runtime hint 共用同一套 resolver，避免配置 shell、`bash_env` 环境变量覆盖与 auto-confirm 行为漂移。bash 进程环境构造也对齐 Python 在 Windows 下的 `PYTHONUTF8` / `PYTHONIOENCODING` 默认值，同时保留显式覆盖。
- 内置控制工具（`task_finish`、`ask_user`、`todo_write`），其中 TODO 管理已对齐 Python 风格的 payload 校验、自动 id、status / priority 默认值和 timestamp 保留；核心 workspace 工具（`list_files`、`file_info`、`read_file`、`write_file`、`file_str_replace`、`workspace_grep`、`read_image`，且 image message 注入仅限 `native_multimodal` 任务；`read_file` 已补 Python 风格数字字符串行号解析；`list_files` 也已补 Python 风格数字字符串限制参数、隐藏文件过滤、本地 ripgrep fast path 和 scan-limit 估算 payload；`write_file` / `file_str_replace` 已补 Python 风格标量文本参数转字符串；`file_str_replace` 已补 Python 风格数字字符串替换上限解析；`workspace_grep` 已补真正正则搜索、glob 过滤、标量文本参数转字符串、配置 workspace backend 搜索、本地 ripgrep JSON fast path 和数字字符串限制/上下文参数解析，content 已改为文本摘要，结构化 matches 保留在 metadata，并补齐 Python 风格文本截断和结构化 payload 上限；grep 指向单个文件时也会像 Python 一样绕过隐藏/忽略目录过滤）；通过 `compress_memory` 记录 memory notes；以及支持捕获输出、Python 风格 replacement 解码、stdin、数字字符串 timeout、通过 `bash_shell` metadata 选择 shell、前台超时转后台、后台轮询和后台命令终态 listener 自动通知的 `bash` / `check_background_command` 命令工具；`BackgroundSessionManager::start` 也已对齐 Python 的 manager 级命令启动路径，支持 shell 准备、stdin、auto-confirm 和进程环境覆盖；adopt 已运行进程时也支持显式 `started_at` 时间；后台命令 listener 的失败也会被隔离，单个订阅者异常不会阻止其他订阅者收到终态事件。
- 与 Python 一致的 workspace 路径安全策略：`LocalWorkspaceBackend` 默认拒绝访问 workspace 外路径，会先展开 `~/...` 再执行同一套安全检查；文件、图片、grep 和 bash 工具仍支持可信任务通过 metadata 显式放行。Tool context 会合并 `ExecutionContext.metadata` 和 task metadata，并保持 task metadata 优先级更高，对齐 Python runtime integration 行为。
- `workspace/` 已按 Python 的 base / local / memory / s3 层级拆分，同时继续从 crate 顶层导出 `FileInfo`、`WorkspaceBackend` 和各个具体 backend。
- Python 风格 workspace backend：`LocalWorkspaceBackend` 和 `MemoryWorkspaceBackend` 支持基于 base 的 `**` glob 匹配、稳定的 POSIX 风格路径输出、内存目录元数据，并在读取缺失的内存文件时返回 `NotFound` 错误。`S3WorkspaceBackend` 已接入 Rust `object_store` S3 客户端，支持 S3-compatible bucket、workspace prefix、append、glob listing、metadata lookup 和 Python 风格带点 suffix。Workspace backend 类型也已从 crate 顶层导出。
- 与 Python 一致的 `read_file` 大文件响应限制：超出行数 / 字符数限制时返回文件统计、请求大小、限制值和建议行范围，不再把大文件直接塞进 LLM 上下文。
- 与 Python 一致的 tool-call batch directive 处理：当某个工具请求用户输入或结束任务时，同一轮 LLM response 里后续工具调用会被记录为 skipped result，而不是从 transcript 中消失。
- Python 风格 runtime cancellation controls：可 clone 的 `CancellationToken` 支持幂等取消、callback 注册、父子 token 传播；`RuntimeRunControls` 会在 cycle 前和 tool call 之间检查取消状态，返回 failed result 并发出 `run_cancelled` 事件。
- `RuntimeRunControls` 已支持 Python 风格 before-cycle message provider 和 interruption message provider：调用方可以在每轮 cycle 开始时、memory compaction 和 LLM planning 前注入消息，也可以在一个工具执行完成后中断当前 tool-call batch，让后续工具以 `skipped_due_to_steering` 记录，并把插话消息带入下一轮 cycle。
- 已补 Python 风格 `ExecutionContext`：包含 cancellation token、stream callback、state store 和 metadata 字段；runtime 取消检查现在同时支持 context 内的 token 和 `RuntimeRunControls` 上直接传入的 token；context metadata 会传入工具执行上下文；stream callback 也已透传到 `vv-llm` streaming completion，并可通过 `AgentSDKOptions` 配置。
- 已补 Python 风格 runtime backend helper：`InlineBackend`、`ThreadBackend`、`CeleryBackend` 和可序列化的 `RuntimeRecipe` 覆盖有序 `parallel_map`、thread `submit`、Celery inline fallback、distributed runtime recipe 数据结构，以及带 cancellation / max-cycles 结果的 `execute` cycle loop。`AgentRuntime` 也通过同一 backend abstraction 执行，不再维护一套独立内部循环，并会把该 backend 传入工具上下文，让同步 batch 子任务像 Python `v-agent` 一样走 `execution_backend.parallel_map`。
- 已补参考 Python `runtime.state`、`runtime.stores.sqlite` 和 `runtime.stores.redis` 的 checkpoint store：`Checkpoint`、`InMemoryStateStore`、`SqliteStateStore` 可持久化 messages、cycles、status 和 shared_state；`RedisStateStore` 也已对齐 Python 的 `vv_agent:checkpoint:{task_id}` key 形状，为 Celery 相邻的 distributed / resumable execution 铺好状态层。
- SDK session 也已接入 Python 风格 cancellation：`cancel()` 和可 clone 的 `SessionCancellationHandle` 会把 active cancellation token 透传到 runtime，清空 steering / follow-up 队列，并向 listener 发出 `session_cancel_requested`。
- `create_sub_task` / `sub_task_status` 已接入 runtime-backed sub-agent：配置在 `AgentTask.sub_agents` 里的子 Agent 可以同步运行，也可以通过 `wait_for_completion=false` 异步启动，支持 batch 聚合和状态 / snapshot 轮询。`create_sub_task` 也支持 Python 风格布尔值兼容转换，`include_main_summary` 和 `wait_for_completion` 可接受 `"true"`、`"0"` 等常见字符串值；`SubTaskRequest::new` 使用与 Python dataclass 一致的默认值，`SubTaskOutcome::to_dict` 也会输出 Python 风格子任务结果 payload。已提供 Python 风格 active sub-agent session registry，暴露 `get_sub_agent_session`、`subscribe_sub_agent_session`、`steer_sub_agent_session`，以及 Python-private 兼容别名 `_register_sub_agent_session` / `_unregister_sub_agent_session` guarded unregister 语义；并支持 `sub_task_status(message=...)` 向 active run 期间临时注册的 session 排队 steering message，或继续 `SubTaskManager` 已 attached 的完成 session；同时支持 Python 风格 `wait_for_response` 布尔值兼容转换，并拒绝继续 max-cycles 任务。`SubTaskManager::attach_session` 也会跟踪 Python 风格 session event snapshot，包括 recent activity、latest cycle / tool-call metadata 和可见 workspace 文件列表。Runtime-backed 子任务现在由 session 驱动，仅在运行中临时注册，因此已完成的异步子任务也能通过 `sub_task_status(message=...)` 继续执行，并保留上一轮 messages 和 shared_state，同时避免 stale global session 泄漏。`SubTaskManager::submit` 现在会像 Python 一样拒绝同一 task_id 的运行中重复提交，而不是覆盖 active record。已 attached 的 runtime-backed 子 Agent session 会在后续继续运行时保留 Python 风格 resolved backend/model 元信息，即使 continuation outcome 没有重复这些 metadata，`sub_task_status` 仍能看到原始模型解析结果。`SubTaskManager::get` 和 `wait_for_record` 也提供 Python 风格直接记录检查能力，但返回只读 `ManagedSubTaskSnapshot`，不会暴露线程句柄。继续已完成 session 前会像 Python 一样先清洗 stale resume messages，移除空 assistant、thinking-only assistant 和尾部未闭合 tool calls。
- 子 Agent model / backend 解析已对齐 Python 安全行为：子 Agent 指定不同模型时必须配置 runtime `settings_file`，否则子任务会显式失败，而不是静默复用父 LLM client；配置 settings 后会复用顶层同一套 `vv-llm` settings builder 来解析 backend / model。
- 自动生成的子 Agent prompt 现在会继承父任务的 Python 风格 prompt builder options，并把 `system_prompt_sections` 写入 task metadata，便于 prompt cache 和后续上下文处理保持一致。
- Python 风格的 `activate_skill`：允许的 inline skill 和 `SKILL.md` location 会加载 instructions，更新 `active_skills`，并记录 activation history。
- Python 风格 SDK session continuation 基础能力：`AgentSession::follow_up` 会在 completed run 后自动追跑 queued follow-up，`steer` 在 `continue_run(None)` 中优先于 follow-up，`clear_queues` 可清空待处理 prompt，`query` 会返回最终回答或带具体状态的错误，例如 `status=wait_user`。Session 也支持 listener 注册，能够收到 `session_run_start`、`session_run_end`、`session_follow_up_queued`、`session_steer_queued` 等队列和运行生命周期事件，`AgentRun::to_dict` 会输出 Python 风格 status 细节、todo list、resolved model metadata，以及聚合和逐 cycle 的结构化 token usage。Runtime-backed session 会把 `tool_result` 等 runtime 事件转发给 session listener；listener 可通过可 clone 的 `SessionSteeringHandle` 在运行中排队 steering，runtime 会在下一轮 cycle 前注入该 prompt，或在当前 tool batch 后中断剩余工具调用，记录 `skipped_due_to_steering`、`session_steer_interrupt` 和 `run_steered`。Session 还会自动订阅 `bash` / `check_background_command` 报告的运行中后台命令，终态事件会发出 `background_command_completed` / `background_command_terminal`，并在 run 活跃时把系统通知排队为 steering；background session snapshot 也会保留 Python 稳定的 `shell` 字段形状，未记录 shell 时为 `null`。Runtime-backed session 会把上一轮 messages 和 shared_state 传给后续 prompt，因此 follow-up turn 会延续同一段对话和 TODO / memory 状态，而不是重新开始一个空任务。SDK 创建的 runtime session 也会继承 `AgentSDKOptions.workspace` 和 `AgentSDKOptions.log_preview_chars`，同时作用于 session state、工具执行上下文和转发的 runtime event preview。SDK 启动级 `bash_shell`、`windows_shell_priority`、`bash_env` 也会像 Python 一样合并到 agent task metadata，其中 agent 自己的环境变量配置优先。Session 现在还会把稳定的 `session_id` 写入每次 task metadata；需要固定 id 时可用 `AgentSDKClient::create_session_with_id` 或 `create_agent_session_with_id`，默认生成的 session id 也已对齐 Python 的 12 位 hex 形状。Session constructor 和 `create_agent_session_with_shared_state` 等 helper 可传入初始 shared_state，并仍会自动补 Python 默认的 `todo_list: []`。`AgentSDKClient::create_session_with_workspace` 支持按 session 覆盖 workspace，并同步作用于 session state、runtime workspace metadata 和文件工具 backend；`AgentSession` 也暴露 Python 风格只读 accessor：`agent_name`、`definition`、`workspace`、`messages`、`shared_state`、`latest_run`、`running`。one-shot SDK run 也可通过 `run_with_agent_in_workspace`、`run_agent_in_workspace` 或 `run_in_workspace` 按次覆盖 workspace，对齐 Python `run(..., workspace=...)`。Session 还会跨 turn 复用同一个 `SubTaskManager`，因此后续 prompt 可以继续检查或继续同一 session 里前一轮创建的异步子任务。
- SDK session 已补直接覆盖 Python 多工具 wait-user 续跑场景：同一轮里第一个 `ask_user` 会让 run 暂停，后续 tool call 会记录为 skipped，随后 `continue_run(Some(...))` 会沿用同一段 messages / shared_state 继续执行到完成。
- `AgentSDKClient::query` 已对齐 Python client query 语义：completed run 返回最终回答，非 completed 状态会用 `status=wait_user` 这类 snake_case 状态值报告错误原因。命名 Agent 查询也提供 `query_agent`、`query_agent_with_require_completed` 和 workspace 专用 query helper，对齐 Python `query_agent(..., require_completed=...)`。
- `AgentSDKClient` 会自动发现 `.vv-agent/agents.json` 里的命名 profiles，提供 `list_agents`，并可通过 `run_agent` 按名称运行，同时在 `AgentRun.agent_name` 中保留 profile 名称。普通 `run()` 已对齐 Python 的选择语义：优先使用 default agent，只有一个 profile 时自动选择；未配置 profile 或存在多个 profile 时返回清晰错误。
- Runtime-backed 子 Agent session 会继承父 run 的 LLM stream callback，因此嵌套 Agent 执行中的 provider streaming 事件也会像 Python 一样继续向外转发。子事件会补齐 `task_id`、`session_id`、`sub_agent_name`，并向父级 log / event handler 发出对应的 `sub_agent_*` 事件。
- `runtime` 模块也补齐了 Python 风格公开名称，包括 `InlineBackend`、`CancelledError`、`ManagedSubTask`，并继续导出 hook、state store、cancellation、cycle runner 和 tool runner 类型。
- Memory token utilities 会优先使用 `vv-llm::utilities::count_tokens` 的 tokenizer；不支持的模型继续使用 Python 风格 CJK-aware 估算；结构化 JSON payload 会先序列化再估算，message 级 fallback 也会走 `Message::to_openai_message(true)`，因此多模态 user image block 会保持 Python OpenAI-compatible payload 形状；并暴露基于 settings 的 `resolve_model_token_limits` / `resolve_model_token_limits_from_file` helper，通过 crates.io `vv-llm` settings model 读取模型 context / output token 预算。`memory::COMPACTABLE_TOOLS` 也已导出和 Python 一致的默认 microcompact 工具 allowlist。
- SDK one-shot run 现在不再必须预先传入 runtime：默认会根据 `AgentSDKOptions.settings_file` 构造 `vv-llm` backed runtime；测试和嵌入方也可以注入 `LlmBuilder` 使用确定性 client。`AgentSDKOptions.runtime_hooks`、`log_handler`、`tool_registry_factory`、`execution_backend`、`debug_dump_dir` 和自定义 `resource_loader` 会作用于 SDK 流程，对齐 Python SDK 的扩展点。SDK 自动构建的 runtime 会把 vv-llm resolved token limits 写入 `model_context_window` 和 `reserved_output_tokens` metadata，但调用方显式传入的同名 metadata 优先。模块级 one-shot helper `run_with_options_and_agent` / `query_with_options_and_agent` 对齐 Python `sdk.run(...)` / `sdk.query(...)`，同时保持 Rust 显式参数风格。`AgentSDKClient::run_agent_with_request` / `run_with_agent_request` 暴露和 session 相同的 one-shot request 路径，可在不创建长 session 的情况下传入 shared state、initial messages、cancellation、steering 和 per-run metadata。
- SDK task preparation 现在会在未提供 raw `system_prompt` 时，根据 `AgentDefinition.description` 构建 Python 风格 prompt bundle，并保留生成的 `system_prompt_sections` metadata，方便 prompt cache 和调试链路继续对齐。`prepare_task_for_agent` 也已暴露命名 profile 的 task 预览路径；`AgentSDKClient::new_with_agent`、`new_with_agents`、`prepare_task` 和 `prepare_task_in_workspace` 也覆盖了 Python 默认 agent / 唯一 profile 的 task preparation 入口，同时保留 Rust 显式方法名。`system_prompt_template` 会像 Python 一样替换 agent definition 文本，但仍走完整 prompt builder。相对 `skill_directories` 会按 SDK workspace 解析，因此 `skill_directories=["skills"]` 在 task preparation 和 one-shot run 中会像 Python 一样自动生成 `<available_skills>` prompt。SDK task preparation 也会 clamp 无效的 `max_cycles`、memory compaction threshold 和 memory threshold percentage，保持 Python 兼容的安全范围。SDK prepare、one-shot run 和 session flow 现在都会生成 Python 风格唯一 task id（`agentName_<8 hex>`），避免连续 SDK 运行时 checkpoint / session-memory scope 撞在同一个固定 id 上。SDK session 使用和 one-shot run 一致的 effective agent definition，因此 startup shell defaults、bash env merge、prompt templates 和自动发现的 skill directories 都会作用到 session prompt。
- SDK client 现在可以通过 `create_default_session*` helpers 按 default agent 或唯一已注册 profile 创建 session，也可以按 profile 名称创建 session，不再需要手动复制 `AgentDefinition`，对齐 Python `client.create_session(...)` 的选择语义，同时保留 Rust 显式方法名。
- 基于 `AgentTask` flags 的 Python 风格工具规划，以及 `.vv-agent` 下 `agents.json`、prompt templates 和 skill directories 的资源发现；`agents.json` 已支持完整 agent 字段，包括 sub-agent definitions、tool flags、shell defaults、metadata 和资源路径。资源路径会像 Python 一样展开 `~`，`AgentResourceLoader::discover_force_reload` 可在磁盘资源变更后刷新缓存；SDK client 也可注入自定义 `AgentResourceLoader`，从非默认资源根发现 agents 和 prompt templates。`.vv-agent/hooks` 下的 Python hook 文件会暴露在 Python 风格的 `DiscoveredResources.hooks` 字段，同时保留 `hook_files` 作为 Rust 兼容别名，并通过 diagnostics 报告；Rust hook 执行使用 `AgentSDKOptions.runtime_hooks` 注入。
- SDK 客户端、工具注册表、工作区后端，以及共享协议类型。
- 覆盖公开 API 构造、Rust SDK 使用、vv-llm 集成、runtime 工具循环、schema parity 和 workspace 工具的 smoke tests。

对 Python 实现的更深层 parity 仍待继续补齐，包括生产级 distributed-worker integration 和 provider-specific 请求序列化等边界行为。
迁移目标是尽最大可能照搬 Python `v-agent` 的能力、实现形状和行为，而不是只提供一个最小 Rust wrapper。
