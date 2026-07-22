# vv-agent-rs

[English](README.md)

`vv-agent-rs` 是 `vv-agent` crate 的 Rust 工作空间，提供可嵌入的 Agent
运行时、SDK、CLI、工具系统、记忆层和工作区抽象，用来构建由大语言模型驱动的自动化任务。

它的核心设计是显式控制 Agent 状态。向后兼容的默认方式仍是调用 `task_finish` 完成任务、
调用 `ask_user` 等待用户输入；宿主也可以显式配置 `NoToolPolicy::Finish` 或
`NoToolPolicy::WaitUser`，让普通 assistant 回复结束或暂停运行。框架只执行声明的策略，
不会根据文本是否“像最终答案”猜测任务是否完成。

## 架构

```text
AgentRuntime
├── LLM client              # 基于 vv-llm 的聊天客户端、endpoint 解析、streaming
├── CycleRunner             # 单轮模型调用：prompt、response、tool-call plan
├── ToolOrchestrator        # 工具 policy、approval、dispatch、timeout、telemetry
├── RuntimeHookManager      # LLM、工具、memory 前后的 hook
├── MemoryManager           # 上下文预算、压缩、artifact、session memory
├── RunHandle / RunEvent    # live 控制、typed event、event-store replay
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
cargo test -p vv-agent -- --test-threads=1
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
单次运行还可以覆盖工具 registry factory、before-cycle / interruption 消息、
sub-task manager、runtime observer、日志预览长度和 LLM 请求 debug dump。完整优先级与
语言适配见 `docs/runtime-control.md`。

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
                .max_handoffs(4)
                .execution_mode(ExecutionMode::Inline)
                .build(),
        )
        .await?;
    println!("{:?}", result.final_output());
    Ok(())
}
```

Handoff 是 Runner 外层的控制权转移，不是 agent-as-tool 调用。目标 Agent 会重新解析
自己的 model 和 model settings，同时沿用当前 session、cancellation token，并继承源
Agent 已修改的 shared state。`max_handoffs` 默认值为 `10`，它独立于
`max_cycles` 限制控制转移次数；审批恢复也保持相同语义。

无工具完成是显式宿主配置。优先级依次为单次 `RunConfig`、Runner 默认配置、Agent 配置，
全部省略时保持 `NoToolPolicy::Continue`：

```rust
use vv_agent::{Agent, NoToolPolicy, RunConfig};

let natural_answer_agent = Agent::builder("assistant")
    .instructions("根据已有上下文回答。")
    .no_tool_policy(NoToolPolicy::Finish)
    .build()?;
let force_tool_driven_run = RunConfig::builder()
    .no_tool_policy(NoToolPolicy::Continue)
    .build();
```

可通过 `RunResult::completion_reason()`、`completion_tool_name()` 和
`partial_output()` 区分自然完成、工具完成、等待、取消、失败和达到最大轮数。

需要跨多次运行复用的默认值应放在
`Runner::builder().default_run_config(...)`。Provider 优先级为 per-run、
Runner；Model 优先级为 per-run、Agent、Runner、当前 Provider 默认模型；
ModelSettings 按 Provider、Runner、Agent、per-run 逐层合并，后面的层覆盖前面的
字段。单次运行替换 Provider 时，不会错误复用 Runner 中绑定到另一 backend 的模型。

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

### Live Run 与事件

`Runner::run()` 和 `run_with_config()` 适合普通一次性调用。应用如果需要给 UI
或服务端提供实时控制，应使用 `Runner::start()`：同一个 `RunHandle` 可以订阅事件、
审批工具、取消运行并等待最终结果。`Runner::stream()` 是基于 `start()` 的 typed live
event 便捷入口。

```rust
use vv_agent::{ApprovalDecision, RunConfig, RunEventPayload};

let handle = runner
    .start(&agent, "检查 workspace 并汇报发现。", RunConfig::default())
    .await?;
let mut events = handle.events();

while let Some(event) = events.next().await {
    match event?.payload() {
        RunEventPayload::AssistantDelta { delta } => print!("{delta}"),
        RunEventPayload::ToolCallStarted { tool_name, .. } => {
            eprintln!("tool started: {tool_name}");
        }
        RunEventPayload::ApprovalRequested { request_id, .. } => {
            handle.approve(request_id, ApprovalDecision::allow()).await?;
        }
        _ => {}
    }
}

let result = handle.result().await?;
```

每个 `RunEvent` 都是 v1 envelope，包含 `event_id`、`run_id`、`trace_id`、
可选 session/parent 标识、时间、metadata 和 typed `RunEventPayload`。
`JsonlRunEventStore` 可以 append 事件并 replay 一个 run，也可以通过 parent run id
带出子事件。

实时工具审批使用 `ApprovalProvider` 和 handle 持有的 broker。面向模型的 `ask_user`
工具仍用于在对话中请求用户输入。宿主应用还可以通过 `ContextProvider` 注入有序 prompt
片段，通过 `MemoryProvider` 接入外部 search、save 和 compaction lifecycle。

`ToolPolicy` 提供 `Default`、`Always`、`Never` 和 `OnRequest` 四种审批模式。
`Default` 继续继承下一层配置；显式 `OnRequest` 按每个工具的静态或动态审批声明决定。
`Always` 强制审批，`Never` 跳过审批，二者都不会执行动态工具审批 predicate。

### 工具能力元数据与执行遥测

工具可以用 `ToolMetadata` 声明可选的、仅宿主可见的能力信息。通过
`FunctionTool::builder(...).tool_metadata(...)`（或
`StaticTool::with_tool_metadata`）附加声明，再用 `ToolPolicy` 的累加拒绝方法收紧一次运行：

```rust
use serde_json::Value;
use vv_agent::{
    FunctionTool, RunConfig, ToolIdempotency, ToolMetadata, ToolOutput, ToolPolicy,
    ToolSideEffect,
};

let inspect = FunctionTool::builder("inspect_source")
    .description("Inspect a source file.")
    .tool_metadata(ToolMetadata {
        side_effect: ToolSideEffect::Read,
        idempotency: ToolIdempotency::Supported,
        terminal: false,
        capability_tags: vec!["source.inspect".to_string()],
        cost_dimensions: vec!["workspace.bytes_read".to_string()],
    })
    .handler(|_context, _arguments: Value| async {
        Ok(ToolOutput::text("inspection complete"))
    })
    .build()?;

let policy = ToolPolicy::default()
    .deny_side_effect(ToolSideEffect::Write)
    .deny_capability_tag("secrets.read")?
    .deny_terminal_tools()
    .deny_cost_dimension("workflow.credit")?;
let run_config = RunConfig::builder().tool_policy(policy).build();
```

`side_effect` 只是一个没有层级关系的粗粒度声明。`terminal=true` 只表示工具可能返回
`finish` 或 `wait_user`，不会自行结束运行。`capability_tags` 和
`cost_dimensions` 会规范化并按完整字符串精确匹配；cost dimension 不是价格、用量观测
或运行预算。类型化声明与工具的通用 `metadata` 相互独立，也不会进入模型可见的 function
schema。

元数据拒绝会与已有的工具名、参数、审批、planned-name、预算和 runtime 检查共同生效。
Agent、Runner 默认值和单次运行的拒绝集合取并集（`deny_terminal_tools` 使用逻辑 OR）；
configured sub-agent、agent-as-tool、handoff 和分布式 worker 只能继承并增加拒绝项。
命中后返回 `tool_not_allowed`，且不会启动 executor。未声明 typed metadata、四个新策略字段
保持空列表 / `false` 时，原有工具可用性、schema、completion 和 approval 行为不变。

类型化运行时顺序是 `ToolCallPlanned`、可选 approval 事件、在副作用可能开始前立即产生的
`ToolCallStarted`，以及结果生成后的 `ToolCallCompleted`。completed 事件提供
`directive`、`error_code`、`execution_started` 和 `duration_ms`；执行前被拒绝时没有
started 事件，wire 上是 `execution_started=false`、`duration_ms=null`。生命周期、
持久化与投影细节见[架构](docs/architecture.md)、
[Durable Checkpoint And Resume](docs/checkpoint-resume.md)和
[App Server 协议](crates/vv-agent/docs/app_server.md)。

### 运行预算

`RunConfig::budget_limits` 可以分别限制总 token、未缓存输入 token、工具总调用数、
指定工具调用数、活跃运行时间和宿主计量成本。所有限制都是可选且任务无关的：框架不会
根据 prompt、任务类型、里程碑或答案质量来执行预算。

可以通过 `result.budget_usage()` 和 `result.budget_exhaustion()` 查看结果。预算耗尽会
得到 completion reason 为 `budget_exhausted` 的类型化失败终态，不会伪装成任务成功。
未配置预算时保留原有事件流。详见[运行预算](docs/run-budgets.md)和
`crates/vv-agent/examples/07_token_budget_guard.rs`。

### App Server

当产品宿主需要通过稳定 JSON-RPC 协议驱动 `vv-agent`，而不是直接链接 runtime 内部实现时，
使用 App Server。它支持 stdio JSONL transport、thread / turn 生命周期请求、live item
notification、approval server request、replay、schema 生成和 typed Rust 测试客户端。

```bash
vv-agent app-server --listen stdio
vv-agent app-server schema --out target/app-server-schema/json
vv-agent app-server generate-ts --out target/app-server-schema/typescript
```

协议示例和客户端职责见 `crates/vv-agent/docs/app_server.md`。

### 低层 Runtime

只有在你需要自己组装 LLM client、prompt、工具 registry、workspace 和运行控制时，才直接使用
runtime。新的嵌入式应用应从 `Agent` + `Runner` 开始。

```rust
use std::path::PathBuf;

use vv_agent::config::build_vv_llm_from_local_settings;
use vv_agent::prompt::{build_system_prompt_with_options, BuildSystemPromptOptions};
use vv_agent::types::AgentTask;
use vv_agent::{build_default_registry, AgentRuntime, RuntimeRunControls};

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
| Runtime | 多轮模型执行、显式终态、live `RunHandle`、取消、typed event、event replay 和 max-cycle 控制。 |
| Tools | 内置工具，以及统一处理 policy、approval、dispatch、timeout、telemetry 的 `ToolOrchestrator` 路径。 |
| SDK | `Agent`、`Runner`、`RunConfig`、`ModelSettings`、typed tool、`Agent::as_tool()`、`RunEvent`、provider 和 `Session`。 |
| Memory | Token 预算、prompt-too-long 重试、micro/full compaction、大型工具结果 artifact、图片裁剪、session memory 和外部 provider hook。 |
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

`find_files` 和 `search_files` 针对大 workspace 内置了安全默认值：结果数上限、隐藏目录和依赖目录过滤、
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
handoff、live approval、后台任务、tracing、子 Agent pipeline、skills、streaming、取消、state store、
执行后端、workspace 后端和临时工具注入。

## 真实模型 Smoke Test

真实测试默认关闭，会使用本地 settings 文件，且不会打印凭据。默认读取未跟踪的
`crates/vv-agent/tests/dev_settings.json`；可从
`crates/vv-agent/tests/dev_settings.example.json` 复制。

```bash
VV_AGENT_RUN_LIVE_TESTS=1 \
cargo test -p vv-agent --test live_deepseek -- --ignored
```

live 套件会覆盖直接 runtime 完成、SDK 完成、`ask_user`、todo 更新、memory note、skill
激活、workspace 工具、图片读取、前台和后台 shell 命令、子 Agent 轮询，以及配置化子 Agent 委托。

## 验证

在 `vv-agent-rs/` 下运行标准检查：

```bash
cargo fmt --check
cargo test -p vv-agent -- --test-threads=1
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
