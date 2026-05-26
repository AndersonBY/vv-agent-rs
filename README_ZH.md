# vv-agent-rs

[English](README.md)

VectorVein Agent 库的 Rust 工作空间。这个 crate 尽量贴近 Python `v-agent/src/vv_agent` 的公开表面，让 Rust 调用方先能依赖稳定的顶层 API，而更深层的运行时一致性则按模块逐步补齐。

## 目录结构

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
      runtime/
        mod.rs
        results.rs
        sub_agents.rs
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
          skills/
            mod.rs
            models.rs
            normalize.rs
            parser.rs
            state.rs
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
- 同一 package 内的 CLI 目标。
- 与 Python 包对齐的顶层模块：`background_sessions`、`cli`、`config`、`constants`、`integrations`、`llm`、`memory`、`processes`、`prompt`、`runtime`、`sdk`、`skills`、`tools`、`types` 和 `workspace`。
- 基于 crates.io 官方 `vv-llm = "0.1.0"` 的 chat client 构建，通过 `build_vv_llm_from_local_settings` 解析配置化 endpoint，并把 provider HTTP / 协议处理交给 `vv-llm`；同时保留 `ScriptedLlmClient` 用于确定性测试。
- 一个基础 multi-cycle runtime，可以把 tool schemas 发给 LLM、执行工具调用，并通过 `task_finish` 或 `ask_user` 收敛。
- `runtime/` 已拆成主循环、工具结果解析和 sub-agent 执行模块，让后续继续补齐 Python parity 时改动更集中。
- `tools/` 已按 Python `v-agent` 的结构拆分为 `base`、`registry`、canonical `schemas`、共享 `common` helper 和各个 handler 模块。
- `activate_skill` handler 已拆成模型、解析、归一化和 shared state helper，更接近 Python `v-agent` 的 skill 边界。
- 默认工具 schema 使用参考 Python `v-agent` 的高信息量描述，让模型拿到文件访问、grep、bash / 后台命令、todo、skills、图片和 sub-agent 的完整操作指引。
- 内置控制工具（`task_finish`、`ask_user`、`todo_write`）、核心 workspace 工具（`list_files`、`file_info`、`read_file`、`write_file`、`file_str_replace`、`workspace_grep`、`read_image`）、通过 `compress_memory` 记录 memory notes，以及支持捕获输出、stdin、前台超时转后台和后台轮询的 `bash` / `check_background_command` 命令工具。
- 与 Python 一致的 workspace 路径安全策略：文件、图片、grep 和 bash 工具默认拒绝访问 workspace 外路径，可信任务可通过 metadata 显式放行。
- 与 Python 一致的 `read_file` 大文件响应限制：超出行数 / 字符数限制时返回文件统计、请求大小、限制值和建议行范围，不再把大文件直接塞进 LLM 上下文。
- `create_sub_task` / `sub_task_status` 已接入 runtime-backed sub-agent：配置在 `AgentTask.sub_agents` 里的子 Agent 可以同步运行，也可以通过 `wait_for_completion=false` 异步启动，支持 batch 聚合和状态 / snapshot 轮询。
- Python 风格的 `activate_skill`：允许的 inline skill 和 `SKILL.md` location 会加载 instructions，更新 `active_skills`，并记录 activation history。
- 基于 `AgentTask` flags 的 Python 风格工具规划，以及 `.vv-agent` 下 `agents.json`、prompt templates 和 skill directories 的资源发现；`agents.json` 已支持完整 agent 字段，包括 sub-agent definitions、tool flags、shell defaults、metadata 和资源路径。
- SDK 客户端、工具注册表、工作区后端，以及共享协议类型。
- 覆盖公开 API 构造、Rust SDK 使用、vv-llm 集成、runtime 工具循环、schema parity 和 workspace 工具的 smoke tests。

对 Python 实现的更深层 parity 仍待继续补齐，包括 hooks、完整 memory compaction、更完整的 sub-agent session 管理与 steering、distributed backends 和剩余内置工具。
迁移目标是尽最大可能照搬 Python `v-agent` 的能力和行为，而不是只提供一个最小 Rust wrapper。
