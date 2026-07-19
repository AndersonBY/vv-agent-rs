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
        "kimi-k2.6": {
          "id": "kimi-k2.6",
          "endpoints": [
            {
              "endpoint_id": "moonshot-default",
              "model_id": "kimi-k2.6"
            }
          ],
          "context_length": 128000,
          "max_output_tokens": 16384,
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

`load_llm_settings_from_file()` also supports Python-style settings literals
used by older templates, but new checked-in fixtures should prefer JSON unless
there is a concrete need for a Python source fixture.

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
let model = ModelRef::backend("moonshot", "kimi-k2.6");
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

## Streaming Usage Accounting

OpenAI-compatible request serialization remains owned by `vv-llm`. This
repository requires `vv-llm` 0.4.3 or newer so streaming calls request the
provider's final usage chunk by default and reasoning-only assistant history
keeps an explicit empty OpenAI-compatible `content` field. Explicit provider
cache details are then projected into `TokenUsage` as `provider_reported`; if
the provider omits them, the runtime keeps cache accounting unavailable instead
of manufacturing a zero. Keep request-body regressions in `vv-llm`; the Agent
runtime tests only the provider-neutral message and usage projection.

## Exact Model Resolution

Model keys are exact. `resolve_model_endpoint(settings, backend, model)` asks
`vv-llm` to resolve the requested key under
`LLM_SETTINGS.backends.<backend>.models`.

Do not add aliases between independent provider models. For example,
`kimi-k2.5` and `kimi-k2.6` are separate model ids. If only `kimi-k2.6` is
configured, requesting `kimi-k2.5` must fail instead of silently using
`kimi-k2.6`.

This behavior is covered by `tests/vv_llm_integration.rs`.

## Kimi K3 Request Profile

`kimi-k3` always uses its provider-defined reasoning and sampling profile. The
LLM bridge sends top-level `reasoning_effort="max"`, omits `temperature`,
`top_p`, fixed penalty/count fields, and legacy K2.x `thinking` controls, and
maps public `max_tokens` / `max_output_tokens` to the provider's
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
| CLI `--model` | `kimi-k2.6` |
| Examples `V_AGENT_EXAMPLE_BACKEND` | `moonshot` |
| Examples `V_AGENT_EXAMPLE_MODEL` | `kimi-k2.6` |
| Live Moonshot `VV_AGENT_LIVE_MODEL` | `kimi-k2.6` |

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
