# vv-agent-rs

[English](README.md)

VectorVein Agent 的 Rust 工作空间，包含运行时、SDK、CLI、内置工具和工作区后端。这个 crate 应该作为独立 Rust 包使用：面向模型的 prompt 和工具 schema 聚焦可执行能力、约束和输入要求。

## 目录结构

```text
vv-agent-rs/
  Cargo.toml
  crates/vv-agent/
    src/
      config, constants, integrations, llm, memory, prompt, runtime, sdk,
      skills, tools, types, workspace, cli
    tests/
      public API, runtime, SDK, LLM, tools, workspace, skills, CLI, examples,
      and live smoke coverage
```

包名是 `vv-agent`；库目标以 `vv_agent` 导入，符合连字符包名的 Rust 命名规则。

## 验证

在 `vv-agent-rs/` 目录下运行：

```bash
cargo fmt --check
cargo test
cargo check --examples
cargo clippy --all-targets --all-features -- -D warnings
```

真实 DeepSeek smoke test 默认关闭，会使用本地 vv-llm 配置文件，且不会打印凭据。当前
live 套件会验证直接 runtime 完成、SDK 完成、workspace `write_file`/`read_file`、`file_str_replace`、`list_files`、`workspace_grep` 和 `file_info` 工具调用、后台命令 handoff，以及通过 `create_sub_task` 调用配置好的子 Agent：

```bash
VV_AGENT_RUN_LIVE_TESTS=1 \
VV_AGENT_LIVE_SETTINGS_JSON=/path/to/dev_settings.json \
cargo test --test live_deepseek -- --ignored
```

## 当前范围

当前 Rust 实现包括：

- 一个 Cargo workspace，包含主 `vv-agent` library 和同包内 `vv-agent` CLI。
- 稳定的 crate 顶层导出，覆盖核心 Agent 类型、运行时执行、工具调度、内置工具注册、SDK client、工作区后端、prompt helper、memory helper 和共享协议类型。
- 基于 crates.io 官方 `vv-llm = "0.2.3"` 的 chat client 构建，支持本地 settings 解析、endpoint 解析、endpoint retry/failover、streaming 事件、prompt-cache metadata、请求 debug dump、模型 token limit 解析和 usage 统计。Provider HTTP 与请求序列化统一交给 `vv-llm`。
- 用于测试的确定性 `ScriptedLlmClient`，支持固定响应 step、callback 响应 step、实时请求检查和脚本耗尽错误。
- 多轮运行时执行，支持 tool-schema planning、tool-call dispatch、完成/等待用户收敛、runtime hooks、取消、生命周期事件、before-cycle 消息注入、插话中断和 max-cycle 处理。
- inline、thread 和 checkpoint-dispatched 执行后端，配套可序列化 runtime recipe，以及 memory、SQLite、Redis 状态存储。
- Prompt 构建能力，包含结构化 sections、stable prompt hash、本地化工具指引、可用 skill 渲染、子 Agent 指引、prompt-cache break tracking、当前时间 section 和 session memory 注入。
- Memory 管理能力，包含上下文预算、usage 估算、大型工具结果 artifact 压缩、microcompaction、完整摘要、图片 payload 裁剪、重复压缩、session memory、prompt-too-long 重试和压缩后文件上下文恢复。
- 高信息量内置工具 schema 和 handler，覆盖任务完成、向用户提问、TODO 管理、文件列表、文件元数据、文本读取、写入、字符串替换、grep、图片读取、memory note、前台/后台 shell 命令、skill 激活、子任务创建和子任务状态/续跑。
- 工作区安全策略和后端，支持本地文件、内存文件、S3-compatible object store、稳定路径输出、glob 列表、append、metadata lookup、缺失文件错误、隐藏/忽略过滤，以及可信任务显式访问工作区外路径。
- SDK 流程，支持命名 Agent 发现、任务预览、one-shot run、query helper、长会话、workspace override、shared state、由宿主程序显式传入的 Rust 原生 runtime hook、事件 listener、stream callback、取消、steering、follow-up prompt 和跨 turn session 复用。
- Resource discovery 覆盖 Agent profile、prompt template 和 skill directory。Runtime hook 是使用方显式传入的 Rust `RuntimeHook` 实现。
- Runtime-backed 子 Agent，支持同步或后台执行、批量任务提交、状态 snapshot、steering、已完成 session 续跑、重复运行任务保护和继承父级 stream callback。
- Skill 发现、frontmatter 解析、metadata 归一化、校验、带预算限制的 `<available_skills>` prompt 渲染、激活状态和激活历史。
- 覆盖 SDK/session API、runtime hooks、自定义工具、子 Agent pipeline、skills、streaming、cancellation、state stores、execution backends、workspace backends 和临时工具注入的 checked examples。
- 覆盖公开 API 构造、CLI 任务准备、SDK resources、runtime cycle、tool planning、模型可见 schema 质量、workspace tools、vv-llm 集成和真实 DeepSeek smoke 的测试。

Provider 请求序列化会统一交给 crates.io 官方 `vv-llm` crate；请求侧 provider 行为应优先补到 `vv-llm`。本仓库聚焦 Agent runtime、工具系统、SDK、prompt、memory 和 workspace 执行层。
