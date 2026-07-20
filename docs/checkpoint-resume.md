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

## Tool Metadata And Compatibility

New checkpoint v2 run-definition writers freeze the effective
`tool_metadata` object for every tool, writing `null` when no typed declaration
exists. They retain the legacy `idempotency` field and also freeze
`denied_side_effects`, `denied_capability_tags`, `deny_terminal_tools`, and
`denied_cost_dimensions` in the effective tool policy. Distributed envelopes
carry that already-merged policy; a worker does not create a new permission
layer. Metadata or policy drift fails with `checkpoint_definition_mismatch`
before claim, model calls, or tool effects.

Definitions written by contract 0.7.1 do not contain the additive fields.
Resume first verifies the digest against the exact stored definition, then
adds `tool_metadata=null`, empty denial lists, and
`deny_terminal_tools=false` only to an in-memory comparison copy. It never
rewrites the stored definition or digest. This preserves exact 0.7.1 bytes and
allows an old definition to resume only when the current declaration and
policy are still at those defaults. Non-default current values fail closed.
Generic tool metadata is never promoted during comparison.

Execution telemetry is not a durable receipt. A `tool_call_started` event may
exist without `tool_call_completed` after cancellation, process loss, or an
exception. The checkpoint v2 operation journal remains authoritative for
whether an operation is planned, started, committed, replayable, or ambiguous;
neither `duration_ms` nor a lifecycle observer provides exactly-once effects.

The typed `RunEvent` envelope remains wire version `v1`. Contract 0.8 writers
add `tool_call_planned` and always include `directive`, `error_code`,
`execution_started`, and `duration_ms` on newly written completed tool events.
The reader still accepts legacy completed v1 events without those additive
fields and preserves their absence instead of fabricating execution or timing
facts. Legacy distributed v1 envelopes and checkpoint-v1 namespaces remain
unchanged; additive tool-policy fields on durable v2 transports default to
empty lists and `false` when absent.

With typed metadata omitted and all new denials disabled, model-visible tool
schemas, eligibility, approval, completion, budgets, and external side effects
follow the previous behavior. The new telemetry is observational and does not
change those decisions.

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
cargo test -p vv-agent --test run_events_v1
cargo test -p vv-agent --test run_events_v1_invalid
cargo test -p vv-agent --test runner_producer_parity
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
