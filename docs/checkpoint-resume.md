# Durable Checkpoint And Resume

Checkpoint v8 is an opt-in Runner capability. It preserves the last committed
cycle, operation receipts, budget usage, extension state, event cursor, claim,
lease, and retained terminal result. The language-neutral behavior is defined
by the locked `vv-agent-contract`; this document records the Rust producer and
transport integration rules.

## Public Configuration

Configure `RunConfig.checkpoint_config` with a stable key, a
`CheckpointStore`, and an explicit resume policy. A concrete store handle is
used by the scheduler process. A distributed worker resolves the same logical
store through `RuntimeRecipe.capabilities.checkpoint_store_ref` and its
`DistributedCapabilityRegistry`.

Enabled records require `schema_version=vv-agent.checkpoint.v8` and
`run_definition_schema=vv-agent.run-definition.v5`. Distributed workers accept
only `vv-agent.distributed-run.v5` and return only
`vv-agent.distributed-worker-response.v3`; no other current record or envelope
shape is read or repaired.

`CheckpointConfig` intentionally keeps concrete `store` and reconstructable
`store_ref` mutually exclusive. When a local scheduler handle and a worker
reference are both needed, keep the concrete store in `CheckpointConfig` and
record the stable worker reference in `capability_refs["checkpoint_store"]`.
The distributed recipe must select that same reference.

## Tool Metadata And Policy

The current run-definition writer freezes the effective `tool_metadata` object
for every tool, writing `null` when no typed declaration exists. It also
freezes `denied_side_effects`, `denied_capability_tags`,
`deny_terminal_tools`, and `denied_cost_dimensions` in the effective tool
policy. Distributed envelopes carry that already-merged policy; a worker does
not create another permission layer. Metadata or policy drift fails with
`checkpoint_definition_mismatch` before claim, model calls, or tool effects.

Run-definition readers accept exactly the current closed shape. Missing,
unknown, stale, or malformed fields are rejected; no comparison copy is
synthesized and no stored definition or digest is rewritten.

Execution telemetry is not a durable receipt. A `tool_call_started` event may
exist without `tool_call_completed` after cancellation, process loss, or an
exception. The checkpoint v8 operation journal remains authoritative for
whether an operation is planned, started, committed, replayable, or ambiguous;
neither `duration_ms` nor a lifecycle observer provides exactly-once effects.

The typed `RunEvent` envelope uses the strict current `v4` discriminator.
Readers require every current field, reject unknown fields, and never dispatch
to an older decoder. Checkpoint outbox entries must contain a canonical current
`RunEvent`, match its embedded `event_id`, and match the recorded payload
digest before a checkpoint is accepted.

## Ownership And Terminal Ordering

Only one component owns a claim at a time:

1. Runner admits or creates the checkpoint without claiming a distributed
   cycle.
2. The scheduler emits a `DistributedRunEnvelope::for_checkpoint_cycle()`.
3. The worker claims the cycle, renews its lease, and executes one real
   `AgentRuntime` cycle.
4. A nonterminal cycle is committed and releases the claim.
5. A terminal cycle returns the tagged `CycleDispatchResult::TerminalCandidate`;
   it does not write `terminal_result` and keeps the claim active.
6. The scheduler reloads the authoritative checkpoint, verifies cycle and
   revision, and adopts the claim.
7. The original Runner applies output guardrails, append-once session
   persistence, the durable session observation, terminal outbox staging,
   claimed terminal finalization, event delivery, terminal acknowledgement,
   and only then returns to the host.

Transport payloads are never ownership proof. The only response variants are
`Pending`, `Committed`, `TerminalCandidate`, and `TerminalReplay`; transport
failure is out of band. The scheduler always reloads the store and obtains the
current claim token there. A stale candidate is rejected, and a replay must
exactly match the retained durable result. The old `finished` and terminal
Boolean fields are rejected.

If candidate acknowledgement is lost, the lease expires and the worker uses a
recovery claim. Model and tool receipts are replayed without another external
call. In-flight messages and cycles are reconstructed from those receipts;
only a completed cycle or final terminal commit advances the durable
transcript.

## Model Call Ledger

Every model dispatch attempt admitted across the local provider boundary adds
one `ModelCallRecord` to the checkpoint and to the public
`result.token_usage().model_calls` ledger. The ledger covers `AgentCycle`,
`SessionMemory`, and `MemoryCompaction` operations. Logical retries retain
their `operation_id`, receive a new `call_id`, and increment `attempt`; failed
and ambiguous attempts remain visible. Replaying a durable receipt does not add
another record or charge the budget again.

The terminal `TaskTokenUsage` aggregate is derived from this complete ledger.
Provider-omitted token and cache observations remain `None` with
`AccountingMissing` status; only an explicit provider-reported zero is treated
as zero. Ledger, budget snapshot, terminal model event, and durable operation
transition share the same checkpoint progress boundary.

## Worker Reconstruction

`DistributedCycleWorker::new()` has a production checkpoint-v8 executor. It
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

## Apalis Enqueue Transport

The Apalis bridge is deliberately enqueue-only. `ApalisCycleEnqueuer` requires
only `TaskSink<ApalisCycleJob>` and preserves the envelope idempotency key and
optional not-before time. `run_apalis_worker_task` executes one envelope and
returns its typed worker response to the host.

There is no `WaitForCompletion` adapter and no result-polling dispatcher in the
public surface. Hosts persist that worker response and invoke the nonblocking
distributed `advance` callback; terminal candidates remain subject to the
separate framework finalizer and are never transported by a polling wait.

## Deferred Tool Barrier And Resolution

`ToolContext::defer` is the framework-owned factory for an opaque
`DeferredToolHandle`. It can only create a handle after the runtime has
attached checkpoint, operation, attempt, and request-digest identity. A
non-durable run receives a completed `ERROR` result with
`deferred_requires_checkpoint`; no provider call may occur on that path.

The runtime collects the complete model-tool batch and calls one
`CheckpointStore::admit_deferred_batch` CAS. The CAS writes all completed and
deferred journal outcomes and outbox events, sets `CheckpointStatus::Deferred`
when any handle remains unresolved, and releases the active claim exactly
once. A deferred checkpoint is not claimable for another model cycle.

Callbacks call `resolve_deferred(handle, result)` without an expected
revision. Memory, SQLite, and Redis check the independent receipt index first,
then atomically transition the deferred journal entry, insert the receipt
tombstone, stage `tool_call_completed`, and release one barrier item. Exact
replays return the retained receipt; conflicting results return
`deferred_resolution_conflict`, invalid result statuses return
`deferred_resolution_result_invalid`, early-started callbacks return the
retryable `deferred_resolution_not_admitted` decision, ambiguous callbacks
require reconciliation, stale identities return `deferred_resolution_stale`,
and an exact active handle on a claimed checkpoint returns
`deferred_checkpoint_claimed`. The four resolver codes are typed errors, not
`DeferredResolveDecision` variants. Only `SUCCESS` and `ERROR` results are
definitive, and the resolver never invokes the external tool or creates a
terminal result.

Crash recovery accepts trusted exact handles through one
`accept_deferred_batch` CAS under a recovery claim. It writes paired
`reconciliation_resolved` and `tool_call_deferred` events and is idempotent on
an exact replay. Distributed workers retain the existing pending response wire;
the scheduler waits with `deferred_pending` and resumes through the existing
driver after the final receipt releases the barrier. App Server exposes the
state as a non-terminal interrupted turn with `waitReason=deferred_pending`.

## Host Interaction And Controller Commands

`HostInteractionRequest` is a closed, credential-redacted v8 wire value. The
producer binds it to the one active logical-cycle claim and atomically writes
the `host_interaction` checkpoint projection, an active interaction record, a
`host_interaction_requested` event, and the independent UI notification
outbox. A retry with the same interaction identity and digest returns the
retained outcome; a different binding or digest is a zero-write conflict.

`ControllerCommand` is the only control envelope and has five closed variants:
host response, suspend, resume, cancel, and reconciliation abort. Commands
carry the authoritative run and revision fences. A response is admitted as a
full `resolved_pending` record and a durable recovery wake; it does not inject
user input or create a new cycle. The worker must then call
`claim_and_consume_host_interaction_response`, which claims the checkpoint and
record in one CAS, injects the response exactly once, writes
`host_interaction_response_consumed`, releases the transient record claim, and
retains the execution claim for the same logical cycle. Crash-before-commit
retries the pending record; replay-after-commit performs no second injection.

Suspension preserves whether the origin was running or waiting for a host
interaction. Resume dispatches only when the origin is runnable or already has
a pending response. Deferred, ambiguous, and terminal states have precedence
over all controller commands. Cancel and abort create the terminal result only
through the controller transition; host-interaction candidates themselves are
never terminal results.

## Verification

Focused producer tests:

```bash
cargo test -p vv-agent --test run_events_contract
cargo test -p vv-agent --test run_event_validation
cargo test -p vv-agent --test runner_producer_parity
cargo test -p vv-agent --test runner_checkpoint
cargo test -p vv-agent --test distributed_checkpoint
cargo test -p vv-agent --test controller_command
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
