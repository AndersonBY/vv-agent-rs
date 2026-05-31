# vv-agent 示例

[English](README.md)

这些示例覆盖 `vv-agent` 的主要嵌入和运行方式：Agent + Runner、直接
runtime、session、runtime hook、自定义工具、子 Agent、streaming、状态存储、执行后端
和 workspace 后端。

所有命令建议在 `vv-agent-rs` 仓库根目录执行，也就是包含 `Cargo.toml` 的目录：

```bash
cd path/to/vv-agent-rs
```

## 准备配置

大多数示例会通过 `vv-llm` 调真实模型。默认读取：

- `VV_AGENT_LOCAL_SETTINGS=local_settings.json`
- `V_AGENT_EXAMPLE_BACKEND=moonshot`
- `V_AGENT_EXAMPLE_MODEL=kimi-k2.6`
- `V_AGENT_EXAMPLE_WORKSPACE=./workspace`
- `V_AGENT_EXAMPLE_VERBOSE=true`

可以从仓库内的模板复制一份本地配置，真实 key 文件不要提交：

```bash
cp crates/vv-agent/tests/dev_settings.example.json local_settings.json
```

填好 `local_settings.json` 里的 endpoint key 后运行示例：

```bash
V_AGENT_EXAMPLE_MODEL=kimi-k2.6 \
cargo run -p vv-agent --example 01_quick_start
```

如果要直接使用其他 settings 文件：

```bash
VV_AGENT_LOCAL_SETTINGS=crates/vv-agent/tests/dev_settings.json \
V_AGENT_EXAMPLE_MODEL=kimi-k2.6 \
cargo run -p vv-agent --example 03_sdk_client
```

## 示例索引

| 示例 | 重点 |
| --- | --- |
| `01_quick_start` | 直接 runtime、prompt 构建和工具 registry。 |
| `02_agent_profiles` | 使用 `Runner` 的 Agent profile metadata。 |
| `03_sdk_client` | 基于 `Runner` 的 one-shot 调用和 Agent handoff。 |
| `04_session_api` | 基于 `RunConfig` 的长会话 `MemorySession`。 |
| `05_ask_user_resume` | `ask_user` 等待状态和继续执行。 |
| `06_runtime_hooks` | before-LLM / before-tool hook。 |
| `07_token_budget_guard` | token 预算监控和强制收尾。 |
| `08_custom_tool` | 注册并调用自定义工具。 |
| `09_resource_loader` | 从 workspace 加载 Agent、prompt 和 skill 资源。 |
| `10_read_image` | 通过 `read_image` 工具读取图片。 |
| `11_sub_agent_pipeline` | 基于 workspace 文件的子 Agent 协同流程。 |
| `12_skill_activation` | skill 发现和 `activate_skill` 使用。 |
| `13_arxiv_pipeline` | 带预算 hook 的研究型 pipeline。 |
| `14_batch_sub_tasks` | 批量子任务委托。 |
| `15_memory_compact_hook` | memory compaction hook 行为。 |
| `16_hook_composition` | 组合 timing、policy、result hook。 |
| `17_error_recovery` | `Runner` 调用外层重试。 |
| `18_cancellation` | cancellation token 和直接 runtime 执行。 |
| `19_streaming` | streaming callback 收集。 |
| `20_thread_backend` | thread 执行后端。 |
| `21_state_checkpoint` | memory / SQLite 状态存储和 checkpoint 序列化。 |
| `22_sdk_advanced` | threaded execution 等高级 `RunConfig` 选项。 |
| `23_distributed_backend` | 分布式 backend API 和 inline fallback。 |
| `24_workspace_backends` | local、memory、S3-compatible、wrapper workspace 后端。 |
| `25_temporary_tool_injection` | runtime hook 临时注入工具窗口。 |
| `26_agent_runner_facade` | `Agent` + `Runner` 与 `VvLlmModelProvider`。 |
| `27_facade_handoff` | handoff 流程，将控制权转交给另一个 Agent。 |
| `28_facade_approval_background_trace` | approval resume、后台 Agent task 和 JSONL trace exporter。 |

## 验证

检查所有 examples 能编译：

```bash
cargo check --examples
```

检查编号示例是否完整：

```bash
cargo test -p vv-agent --test examples_coverage
```

运行 crate 完整测试：

```bash
cargo test -p vv-agent
```

真实 smoke test 和 examples 分开管理。`VV_AGENT_RUN_LIVE_TESTS` 以及
`crates/vv-agent/tests/dev_settings.json` 的说明见仓库根目录 README。
