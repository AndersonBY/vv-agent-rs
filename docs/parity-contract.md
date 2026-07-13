# Rust Contract Integration

`vv-agent-rs` implements the Rust side of the canonical contract published by
[`AndersonBY/vv-agent-contract`](https://github.com/AndersonBY/vv-agent-contract).
The normative behavior and change workflow no longer live in this repository.

## Pinned Contract

`contract.lock.json` is the machine-readable adoption record. It pins:

- semantic contract version;
- exact central Git revision;
- immutable release artifact URL and SHA-256;
- local vendored snapshot path;
- canonical `SHA256SUMS` digest.

`crates/vv-agent/tests/fixtures/parity/` is generated from that release. It is
committed for offline and reproducible tests, but it is not an editable source
of truth.

## Required Reading

For shared public, model-visible, runtime, persistence, or wire changes, read:

1. `contract.lock.json` in this repository;
2. `../vv-agent-contract/AGENTS.md`;
3. `../vv-agent-contract/docs/parity-contract.md`;
4. `../vv-agent-contract/docs/change-workflow.md`;
5. sibling `../vv-agent/docs/parity-contract.md`.

If the sibling checkout is unavailable, use the exact repository and revision
from the lock. Do not infer the current contract from a floating `main` branch.

## Snapshot Commands

Offline verification of the committed snapshot:

```bash
python3 scripts/contract_snapshot.py check
```

Stronger verification against the sibling canonical checkout:

```bash
python3 scripts/contract_snapshot.py check --source ../vv-agent-contract
```

Synchronization is allowed only after the canonical version is committed and
its deterministic release zip exists:

```bash
python3 scripts/contract_snapshot.py sync \
  --source ../vv-agent-contract \
  --artifact /path/to/vv-agent-contract-<version>.zip \
  --artifact-url https://github.com/AndersonBY/vv-agent-contract/releases/download/v<version>/vv-agent-contract-<version>.zip
```

Never repair a contract failure by editing a file under
`crates/vv-agent/tests/fixtures/parity/` or changing only a digest.

## Rust Producer Map

| Contract surface | Rust producer or evidence |
| --- | --- |
| Public API inventory | `crates/vv-agent/src/lib.rs`, `crates/vv-agent/tests/parity_evidence_manifests.rs` |
| System prompt | `crates/vv-agent/src/prompt/`, `crates/vv-agent/tests/prompt_public_api.rs` |
| Built-in tool specification | `crates/vv-agent/src/tools/`, `crates/vv-agent/tests/tool_schema_contract.rs` |
| Agent, Runner, result, live control | `crates/vv-agent/src/agent.rs`, `crates/vv-agent/src/runner/`, `crates/vv-agent/src/run_handle.rs` |
| Delegation and background tasks | `crates/vv-agent/src/tools/background_agent_task.rs`, `crates/vv-agent/src/handoffs.rs`, `crates/vv-agent/src/runtime/sub_agents/` |
| Sessions and stores | `crates/vv-agent/src/sessions.rs`, `crates/vv-agent/src/runtime/stores/`, `crates/vv-agent/tests/session_store_parity.rs` |
| Events and tracing | `crates/vv-agent/src/events.rs`, `crates/vv-agent/src/event_store.rs`, `crates/vv-agent/src/tracing.rs` |
| Distributed runtime | `crates/vv-agent/src/runtime/backends/distributed/`, `crates/vv-agent/src/runtime/checkpoint_codec.rs` |
| App Server | `crates/vv-agent/src/app_server/`, `crates/vv-agent/tests/app_server_contract_parity.rs` |
| Real closure tests | `crates/vv-agent/tests/parity_evidence_manifests.rs`, `crates/vv-agent/tests/runner_producer_parity.rs` |

A fixture parser or private helper test cannot replace a real public producer
test. A field that is declared but ignored by a planner, executor, provider, or
store remains a contract failure.

## Rust Adaptations

The following are API-shape adaptations, not behavioral differences:

- structs, traits, builders, generics, and `Result` map to Python dataclasses,
  protocols, decorators, and exceptions;
- async methods and blocking wrappers may coexist where Python exposes
  synchronous convenience APIs;
- typed deserialization maps to Python `output_type` coercion;
- Apalis adapters map to Python Celery adapters through the same distributed
  envelope, checkpoint, lease, and terminal-state contract;
- Rust `ModelProvider` controls map to Python settings-file and provider
  capabilities.

Add a new adaptation only when both implementations preserve input, output,
safety, persistence, cancellation, and lifecycle semantics.

## Completion Gate

```bash
python3 scripts/contract_snapshot.py check --source ../vv-agent-contract
cargo fmt --all -- --check
cargo test -p vv-agent
cargo check --examples
cargo clippy --all-targets --all-features -- -D warnings
```

Then run the Python gate and the central
`vv-agent-contract/.github/workflows/cross-repository.yml` workflow with exact
contract, Python, and Rust refs. If either implementation is incomplete, keep
the central support matrix at `pending-adoption` or `in-progress`.
