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
      runtime.rs
      sdk.rs
      skills.rs
      tools.rs
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
- 基于 `vv-llm` 的 chat client 构建和配置化 endpoint 解析，同时保留 `ScriptedLlmClient` 用于确定性测试。
- 一个基础 multi-cycle runtime，可以把 tool schemas 发给 LLM、执行工具调用，并通过 `task_finish` 或 `ask_user` 收敛。
- 内置控制工具（`task_finish`、`ask_user`、`todo_write`）、核心 workspace 工具（`list_files`、`file_info`、`read_file`、`write_file`、`file_str_replace`），以及支持捕获输出、stdin、前台超时转后台和后台轮询的 `bash` / `check_background_command` 命令工具。
- SDK 客户端、工具注册表、工作区后端，以及共享协议类型。
- 覆盖公开 API 构造、Rust SDK 使用、vv-llm 集成、runtime 工具循环和 workspace 工具的 smoke tests。

对 Python 实现的更深层 parity 仍待继续补齐，包括 hooks、memory compaction、skills activation、sub-agents、session steering、distributed backends 和剩余内置工具。
