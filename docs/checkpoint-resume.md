# Durable Checkpoint And Resume

Checkpoint v2 is an opt-in Runner capability. It preserves the last committed
cycle, operation receipts, budget usage, extension state, event cursor, claim,
lease, and retained terminal result. The language-neutral behavior is defined
by the locked `vv-agent-contract`; this document records the Rust producer and
transport integration rules.

## Public Configuration

Configure `RunConfig.checkpoint_config` with a stable key, a
`CheckpointStoreV2`, and an explicit resume policy. A concrete store handle is
used by the scheduler process. A distributed worker resolves the same logical
store through `RuntimeRecipe.capabilities.checkpoint_store_ref` and its
`DistributedCapabilityRegistry`.

`CheckpointConfig` intentionally keeps concrete `store` and reconstructable
`store_ref` mutually exclusive. When a local scheduler handle and a worker
reference are both needed, keep the concrete store in `CheckpointConfig` and
record the stable worker reference in `capability_refs["checkpoint_store"]`.
The distributed recipe must select that same reference.

## Ownership And Terminal Ordering

Only one component owns a claim at a time:

1. Runner admits or creates the checkpoint without claiming a distributed
   cycle.
2. The scheduler emits a `DistributedRunEnvelope::for_checkpoint_cycle()`.
3. The worker claims the cycle, renews its lease, and executes one real
   `AgentRuntime` cycle.
4. A nonterminal cycle is committed and releases the claim.
5. A terminal cycle returns a `CycleDispatchResult` with
   `terminal_candidate=true`; it does not write `terminal_result` and keeps the
   claim active.
6. The scheduler reloads the authoritative checkpoint, verifies cycle and
   revision, and adopts the claim.
7. The original Runner applies output guardrails, append-once session
   persistence, the durable session observation, terminal outbox staging,
   claimed terminal finalization, event delivery, terminal acknowledgement,
   and only then returns to the host.

Transport payloads are never ownership proof. The scheduler always reloads the
store and obtains the current claim token there. A stale candidate is rejected.

If candidate acknowledgement is lost, the lease expires and the worker uses a
recovery claim. Model and tool receipts are replayed without another external
call. In-flight messages and cycles are reconstructed from those receipts;
only a completed cycle or final terminal commit advances the durable
transcript.

## Worker Reconstruction

`DistributedCycleWorker::new()` has a production checkpoint-v2 executor. It
resolves the declared model, workspace, toolset, policy, hooks, observers,
budget meter, extensions, and reconciliation provider, then rebuilds an inline
single-cycle `AgentRuntime`. `with_checkpoint_executor()` remains available for
deterministic fault tests and specialized hosts.

Before claiming, the worker verifies the envelope task, model, model settings,
budget, checkpoint policy, tool policy, tool schemas, extension descriptors,
and behavior capability references against the embedded frozen run definition.
A digest match alone is not sufficient.

Apalis attempt metadata is passed to
`DistributedCycleWorker::run_cycle_with_delivery()`. Attempt values greater
than one promote the delivery to recovery without mutating the signed/frozen
envelope.

## Apalis Result Transport

Checkpoint polling cannot carry a terminal candidate because the candidate is
not a durable terminal yet. Polling would wait for an acknowledgement that the
scheduler cannot write until it receives the candidate.

Use `apalis::ApalisResultCycleDispatcher` with a backend implementing both:

- `TaskSink<ApalisCycleJob>`
- `WaitForCompletion<CycleDispatchResult>`

The backend must persist task results across processes, support replay by the
preassigned task ID, and define retention/TTL appropriate for the scheduler's
dispatch timeout. An in-process channel is suitable only for tests.

`ApalisCycleDispatcher` remains the compatibility polling adapter for v1 and
read-only replay of an already retained v2 terminal. It explicitly rejects new
checkpoint-v2 work instead of entering a polling deadlock.

## Verification

Focused producer tests:

```bash
cargo test -p vv-agent --test runner_checkpoint_v2
cargo test -p vv-agent --test distributed_checkpoint_v2
cargo test -p vv-agent --features apalis --test apalis_backend
cargo test -p vv-agent --test app_server_turn_resume
```

Full gate:

```bash
python3 scripts/contract_snapshot.py check --source ../vv-agent-contract
cargo fmt --all -- --check
cargo test -p vv-agent --all-features
cargo check --examples
cargo clippy --all-targets --all-features -- -D warnings
```

