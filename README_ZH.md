# vv-agent-rs

[English](README.md)

`vv-agent-rs` 是 `vv-agent` crate 的 Rust 工作空间，提供可嵌入的 Agent
运行时、SDK、CLI、工具系统、记忆层和工作区抽象，用来构建由大语言模型驱动的自动化任务。

它的核心设计是显式控制 Agent 状态：模型只是写出一段像最终答案的文本，并不代表任务完成；
只有调用 `task_finish` 才会完成任务，调用 `ask_user` 则会进入等待用户输入的状态。这样 CLI、
SDK 会话、后台任务和分布式执行都能使用同一套结果契约。

## 架构

```text
AgentRuntime
├── LLM client              # 基于 vv-llm 的聊天客户端、endpoint 解析、streaming
├── CycleRunner             # 单轮模型调用：prompt、response、tool-call plan
├── ToolCallRunner          # 工具调度和 finish / wait-user / continue 收敛
├── RuntimeHookManager      # LLM、工具、memory 前后的 hook
├── MemoryManager           # 上下文预算、压缩、artifact、session memory
├── RuntimeExecutionBackend # 运行调度
│   ├── InlineBackend       # 默认同步执行
│   ├── ThreadBackend       # 非阻塞任务提交
│   └── DistributedBackend  # checkpoint cycle 与可插拔调度
└── WorkspaceBackend        # 工具访问文件 / 对象存储的边界
    ├── LocalWorkspaceBackend
    ├── MemoryWorkspaceBackend
    └── S3WorkspaceBackend
```

Provider 请求构造、endpoint 通信、重试、streaming delta、token limit、usage 统计和
provider 协议细节统一交给已发布的 `vv-llm` crate。`vv-agent` 专注于 Agent
执行层：prompt、工具、hook、memory、session、workspace 访问和任务编排。

## 安装与配置

在本仓库根目录运行：

```bash
cd vv-agent-rs
cargo test -p vv-agent
```

大多数真实模型示例和 CLI 都读取本地 `vv-llm` settings 文件。带密钥的文件应保持未跟踪：

```bash
cp crates/vv-agent/tests/dev_settings.example.json local_settings.json
# 在 local_settings.json 中填入 endpoint key。
```

默认 settings 路径是 `local_settings.json`。示例可通过 `VV_AGENT_LOCAL_SETTINGS` 覆盖，
CLI 可通过 `--settings-file` 覆盖。

## 快速开始

### CLI

```bash
cargo run -p vv-agent -- \
  --prompt "总结这个仓库" \
  --backend deepseek \
  --model deepseek-v4-pro \
  --settings-file local_settings.json \
  --workspace ./workspace \
  --verbose
```

CLI 参数：

| 参数 | 作用 |
| --- | --- |
| `--prompt` | 必填，用户任务。 |
| `--backend` | `LLM_SETTINGS.backends` 下的 backend key。 |
| `--model` | 选中 backend 下的 model key。 |
| `--settings-file` | 本地 `vv-llm` settings 文件。 |
| `--workspace` | 暴露给 workspace 工具的目录。 |
| `--max-cycles` | 最大运行轮数。 |
| `--language` | prompt 和工具指引语言。 |
| `--agent-type` | 可选 Agent 类型，例如 `computer`。 |
| `--verbose` | 输出每轮运行事件。 |

### Agent + Runner SDK

新嵌入场景优先使用 `Agent` + `Runner`。`Agent` 描述 instructions、model、
tools、handoffs、hooks 和默认值；`Runner` 管理 model provider、workspace 默认值和执行；
`RunConfig` 用来覆盖单次运行，而不改变 Agent 定义，包括用于选择 inline、threaded
或 distributed 执行的公共 `ExecutionMode`。

```rust
use vv_agent::{Agent, ExecutionMode, ModelRef, Runner, RunConfig, VvLlmModelProvider};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let provider = VvLlmModelProvider::from_settings_file("local_settings.json")
        .with_default_backend("deepseek");
    let runner = Runner::builder()
        .model_provider(provider)
        .workspace("./workspace")
        .build()?;

    let agent = Agent::builder("assistant")
        .instructions("你会先规划任务，必要时调用工具，并在完成后调用 task_finish。")
        .model(ModelRef::backend("deepseek", "deepseek-v4-pro"))
        .build()?;

    let result = runner
        .run_with_config(
            &agent,
            "创建 notes.md，写入三个项目要点。",
            RunConfig::builder()
                .max_cycles(12)
                .execution_mode(ExecutionMode::Inline)
                .build(),
        )
        .await?;
    println!("{:?}", result.final_output());
    Ok(())
}
```

Session 会在多次 runner 调用之间保存上下文：

```rust
use vv_agent::{MemorySession, RunConfig};

let session = MemorySession::new("thread-001");
runner
    .run_with_config(&agent, "分析当前 workspace。", RunConfig::builder().session(session.clone()).build())
    .await?;
let result = runner
    .run_with_config(&agent, "继续补充三条后续建议。", RunConfig::builder().session(session).build())
    .await?;
```

### 低层 Runtime

只有在你需要自己组装 LLM client、prompt、工具 registry、workspace 和运行控制时，才直接使用
runtime。新的嵌入式应用应从 `Agent` + `Runner` 开始。

```rust
use std::path::PathBuf;

use vv_agent::config::build_vv_llm_from_local_settings;
use vv_agent::prompt::{build_system_prompt_with_options, BuildSystemPromptOptions};
use vv_agent::{build_default_registry, AgentRuntime, AgentTask, RuntimeRunControls};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (llm, resolved) = build_vv_llm_from_local_settings(
        "local_settings.json",
        "deepseek",
        "deepseek-v4-pro",
        90.0,
    )?;
    let runtime = AgentRuntime::new(llm).with_tool_registry(build_default_registry());
    let system_prompt = build_system_prompt_with_options(
        "You are a reliable execution agent.",
        BuildSystemPromptOptions {
            language: "zh-CN".to_string(),
            use_workspace: true,
            enable_todo_management: true,
            ..BuildSystemPromptOptions::default()
        },
    );

    let mut task = AgentTask::new(
        "demo",
        resolved.model_id,
        system_prompt,
        "读取 workspace README，并总结这个项目。",
    );
    task.max_cycles = 12;

    let result = runtime.run_with_controls(
        task,
        RuntimeRunControls {
            workspace: Some(PathBuf::from("./workspace")),
            ..RuntimeRunControls::default()
        },
    )?;
    println!("{:?}: {:?}", result.status, result.final_answer);
    Ok(())
}
```

完整低层 runtime 示例见 `crates/vv-agent/examples/01_quick_start.rs`，其中包含 event logging。

## 核心能力

| 模块 | 能力 |
| --- | --- |
| Runtime | 多轮模型执行、工具规划、显式终态、取消、streaming、事件日志和 max-cycle 控制。 |
| Tools | 内置 finish/wait-user、TODO、workspace 读写/列表/grep、图片读取、shell 命令、memory note、skill 和 sub-task 工具。 |
| SDK | `Agent`、`Runner`、`RunConfig`、`ModelSettings`、typed tool、`Agent::as_tool()`、typed event 和 `Session`。 |
| Memory | Token 预算、prompt-too-long 重试、micro/full compaction、大型工具结果 artifact、图片裁剪和 session memory。 |
| Hooks | 使用 Rust `RuntimeHook` 检查或修改 LLM 调用、工具调用、memory compaction 和运行生命周期。 |
| Sub-agents | 基于 runtime 的子任务创建、批量提交、后台状态轮询、续跑、steering 和父级 streaming callback 继承。 |
| Skills | Skill 目录发现、frontmatter 解析、校验、带预算的 prompt 渲染、激活和激活历史。 |
| Workspace | Local、memory、S3 object-store 后端统一在 `WorkspaceBackend` 边界下。 |

## 执行后端

公共 SDK 通过 `ExecutionMode` 选择调度方式。底层 runtime backend struct 仍保留给高级集成：

| 后端 | 使用场景 |
| --- | --- |
| `ExecutionMode::Inline` | 默认同步执行，适合普通 CLI、测试和简单嵌入。 |
| `ExecutionMode::Threaded` | 提交任务后不阻塞调用方。 |
| `ExecutionMode::Distributed` | 带 checkpoint 的 cycle 执行，支持可序列化 runtime recipe 和可插拔调度。 |

Checkpointed run 可以把状态存到 memory、SQLite 或 Redis。可选 `apalis` feature 提供
Apalis job bridge，适合已经使用 Apalis worker 的应用：

```bash
cargo test -p vv-agent --features apalis --test apalis_backend
```

分布式 API 也提供 inline fallback，方便本地开发和测试。示例见
`crates/vv-agent/examples/23_distributed_backend.rs`。

## Workspace 后端

所有内置文件工具都会走 `WorkspaceBackend`。这样本地文件、内存文件和 S3-compatible
object storage 可以共享同一套工具契约。

`list_files` 和 `workspace_grep` 针对大 workspace 内置了安全默认值：结果数上限、隐藏目录和依赖目录过滤、
显式 include ignored path，以及本地可用时用 `rg` 加速。

## 示例

编号示例是了解公开 API 的最好入口：

```bash
cargo run -p vv-agent --example 01_quick_start
cargo run -p vv-agent --example 03_sdk_client
cargo run -p vv-agent --example 04_session_api
cargo run -p vv-agent --example 23_distributed_backend
cargo run -p vv-agent --example 24_workspace_backends
cargo run -p vv-agent --example 26_agent_runner_facade
cargo run -p vv-agent --example 27_facade_handoff
cargo run -p vv-agent --example 28_facade_approval_background_trace
```

完整索引见 `crates/vv-agent/examples/README_ZH.md`，覆盖 Agent + Runner、runtime hook、自定义工具、
handoff、approval resume、后台任务、tracing、子 Agent pipeline、skills、streaming、取消、state store、
执行后端、workspace 后端和临时工具注入。

## 真实模型 Smoke Test

真实测试默认关闭，会使用本地 settings 文件，且不会打印凭据。默认读取未跟踪的
`crates/vv-agent/tests/dev_settings.json`；可从
`crates/vv-agent/tests/dev_settings.example.json` 复制。

```bash
VV_AGENT_RUN_LIVE_TESTS=1 \
cargo test -p vv-agent --test live_deepseek -- --ignored
```

live 套件会覆盖直接 runtime 完成、SDK 完成、`ask_user`、TODO 更新、memory note、skill
激活、workspace 工具、图片读取、前台和后台 shell 命令、子 Agent 轮询，以及配置化子 Agent 委托。

## 验证

在 `vv-agent-rs/` 下运行标准检查：

```bash
cargo fmt --check
cargo test -p vv-agent
cargo check --examples
cargo clippy --all-targets --all-features -- -D warnings
```

修改公开文档和示例时，下面两个检查很有用：

```bash
cargo test -p vv-agent --test public_api
cargo test -p vv-agent --test examples_coverage
```

## 仓库结构

```text
vv-agent-rs/
  Cargo.toml
  crates/vv-agent/
    src/
      cli/        # CLI 入口和任务构造
      config/     # LLM settings 加载和模型解析
      llm/        # LLM trait、脚本化测试 client、vv-llm client bridge
      memory/     # compaction、artifact、session memory、token budget
      prompt/     # system prompt section 和 prompt-cache metadata
      agent.rs    # public Agent builder
      runner.rs   # runtime execution 上的 public Runner
      run_config.rs
      model.rs
      model_settings.rs
      sessions.rs
      runtime/    # agent runtime、hook、backend、cancel、sub-agent
      skills/     # skill 发现、解析、校验、激活
      tools/      # registry、schema、dispatcher、内置 handler
      workspace/  # local、memory、S3 workspace backend
    examples/
    tests/
  docs/
```

更多设计说明见 `docs/`，尤其是 `docs/architecture.md` 和 `docs/model-settings.md`。
