# Model Settings

`vv-agent-rs` builds `VvLlmClient` instances from local `vv-llm` settings. The
checked-in template is `crates/vv-agent/tests/dev_settings.example.json`; real
credentials belong in an untracked file.

## Settings Files

Default live-test path:

```text
crates/vv-agent/tests/dev_settings.json
```

Default example path:

```text
local_settings.json
```

Create either file from the checked-in template:

```bash
cp crates/vv-agent/tests/dev_settings.example.json crates/vv-agent/tests/dev_settings.json
cp crates/vv-agent/tests/dev_settings.example.json local_settings.json
```

Do not use settings files from sibling projects. Tests and examples in this
repository should be self-contained.

## Settings Shape

The template uses `vv-llm` settings schema version `2`:

```json
{
  "VERSION": "2",
  "endpoints": [
    {
      "id": "moonshot-default",
      "api_base": "https://api.moonshot.cn/v1",
      "api_key": "replace-with-moonshot-api-key"
    }
  ],
  "backends": {
    "moonshot": {
      "models": {
        "kimi-k3": {
          "id": "kimi-k3",
          "endpoints": [
            {
              "endpoint_id": "moonshot-default",
              "model_id": "kimi-k3"
            }
          ],
          "context_length": 1048576,
          "max_output_tokens": 131072,
          "native_multimodal": true,
          "function_call_available": true,
          "response_format_available": true
        }
      }
    }
  },
  "embedding_backends": {},
  "rerank_backends": {}
}
```

`load_llm_settings_from_file()` accepts JSON, TOML, and Python literal settings
as current input formats. Checked-in Rust fixtures use JSON.

## Agent + Runner Model Provider

`Agent` and `Runner` resolve models through `ModelProvider`:

```rust
use vv_agent::{ModelRef, Runner, VvLlmModelProvider};

let provider = VvLlmModelProvider::from_settings_file("local_settings.json")
    .with_default_backend("moonshot");
let runner = Runner::builder()
    .model_provider(provider)
    .workspace("./workspace")
    .build()?;
let model = ModelRef::backend("moonshot", "kimi-k3");
```

Use `ModelRef::backend(backend, model)` when a call must resolve a specific
`LLM_SETTINGS.backends.<backend>.models.<model>` entry. `ModelRef::named(model)`
uses the active provider default backend and should only be used when that
default is explicit.

`ModelSettings` carries common model-call options such as temperature,
`max_output_tokens`, tool choice, response format, retry, and provider-specific
`extra_body` / `extra_headers`. These settings are part of the public run
contract and are forwarded to runtime request metadata while the runtime
continues to use `vv-llm` for provider transport.

## Runtime Capacity Metadata

Resolved `context_length` and `max_output_tokens` values describe model
capabilities. The Runner projects them into task/request metadata as
`model_context_window` and `model_max_output_tokens`. In particular,
`model_max_output_tokens` is not copied into `reserved_output_tokens` and does
not become an implicit per-request output limit. The same distinction is kept
when a checkpoint is resumed and when a configured sub-agent inherits or
resolves model capabilities.

Memory capacity uses these precedence rules:

1. Context window: positive explicit task `model_context_window`, resolved
   capability, then a derived planning context. The derived prompt capacity is
   the positive configured compaction threshold or `250000`; output reserve
   and the `13000` buffer are added afterward, so the default is `279000`.
   Zero metadata is absent rather than a zero-sized model.
2. Output reserve: effective positive `ModelSettings.max_tokens`, explicit task
   `reserved_output_tokens`, then `16000`.
3. Only the `16000` fallback reserve is reduced when
   `model_max_output_tokens` is smaller. Explicit request and task reserves are
   never capped or raised by the capability.

The prompt capacity subtracts the selected reserve and the default `13000`
auto-compaction buffer from the context window with saturation at zero. The
effective full-compaction threshold is the smaller of that capacity and the
configured task threshold (`250000` when omitted); a known zero capacity stays
zero. This calculation is task-neutral and does not inspect answer content or
task type.

When both warning and microcompact thresholds are crossed, eligible old tool
results are cleared first. Runtime recalculates usage from the changed messages
and appends the optional warning only if that post-microcompact usage remains
eligible.

## Cache Usage Accounting

OpenAI-compatible request serialization remains owned by `vv-llm`. This
repository requires `vv-llm` 0.4.4 or newer so streaming calls request the
provider's final usage chunk by default, reasoning-only assistant history keeps
an explicit empty OpenAI-compatible `content` field, and typed cache readings
reach the Agent bridge.

Generic OpenAI-compatible providers still leave an omitted cache reading
unknown, while an explicit zero is an observed zero. Moonshot is the one
provider-specific exception: when every recognized cache-read field is absent,
`vv-llm` reports `Some(0)` from Moonshot's documented response contract.
Explicit `null` or malformed fields remain unknown. The Agent bridge uses a
temporary normalization copy so OpenAI uncached input is
`prompt_tokens - cached_tokens`, Anthropic keeps its native `input_tokens` as
the uncached portion, and `TokenUsage.raw` remains the unchanged provider
object. Keep request-body regressions in `vv-llm`; the Agent runtime tests the
provider-neutral message and usage projection.

## Exact Model Resolution

Model keys are exact. `resolve_model_endpoint(settings, backend, model)` asks
`vv-llm` to resolve the requested key under
`LLM_SETTINGS.backends.<backend>.models`.

Do not add aliases between independent provider models. For example,
`kimi-k2.5` and `kimi-k3` are separate model ids. If only `kimi-k3` is
configured, requesting `kimi-k2.5` must fail instead of silently using
`kimi-k3`.

This behavior is covered by `tests/vv_llm_integration.rs`.

## Kimi K3 Request Profile

`kimi-k3` always uses its provider-defined reasoning and sampling profile. The
LLM bridge sends top-level `reasoning_effort="max"`, omits `temperature`,
`top_p`, fixed penalty/count fields, and K2.x `thinking` controls, and maps the
explicit public `max_tokens` request setting to the provider's
`max_completion_tokens` field. These invariants are applied after public
`ModelSettings` are merged so provider-specific `extra_body` values cannot
override them. Unrelated `extra_body` fields continue to pass through.

For multi-turn and tool-call requests, every assistant message retains its
complete `reasoning_content`; streamed reasoning deltas are collected through
the end of the provider stream before that message is stored.

## Current User-Facing Defaults

| Surface | Default |
| --- | --- |
| CLI `--backend` | `moonshot` |
| CLI `--model` | `kimi-k3` |
| Examples `VV_AGENT_EXAMPLE_BACKEND` | `moonshot` |
| Examples `VV_AGENT_EXAMPLE_MODEL` | `kimi-k3` |
| Live Moonshot `VV_AGENT_LIVE_MODEL` | `kimi-k3` |

When changing a model default, update all user-facing surfaces together: CLI,
README, examples, live-test docs, tests, and
`crates/vv-agent/tests/dev_settings.example.json`.

## Key Safety

- Do not commit real keys.
- Keep placeholder values in checked-in templates.
- Keep `crates/vv-agent/tests/dev_settings.json` ignored.
- Use `VV_AGENT_LIVE_SETTINGS_JSON` only when a live test needs a non-default
  settings path.
- Live tests must stay opt-in through `VV_AGENT_RUN_LIVE_TESTS=1`.
