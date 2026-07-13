# Embedded Interactive Sessions

Use `Runner::run()` for one-shot work where the caller owns each turn. Use the
interactive facade when an embedded host needs a stable session identity,
live control of an active run, queued follow-ups, and a typed event feed.

## Create A Session

```rust,ignore
use vv_agent::{
    InteractiveSessionOptions, InteractiveAgentClient, MemorySession, ModelRef,
};

let storage = MemorySession::new("desktop-session-42");
let client = InteractiveAgentClient::new(runner);
let session = client
    .create_session(
        agent,
        InteractiveSessionOptions::new()
            .session_id("desktop-session-42")
            .session(storage),
    )
    .await?;

let mut events = session.subscribe();
let result = session.prompt("Inspect the workspace").await?;
```

If both `session_id` and a `Session` are supplied, their identifiers must
match. If neither is supplied, the facade creates a `MemorySession` with a
generated stable identifier. Existing session items are hydrated when the
facade is created, and `Runner` appends each turn's canonical result messages
to the same storage.

## Live Control

- `active_run_handle()` returns a clone of the current `RunHandle`, including
  approval and state APIs.
- `steer(text)` queues a high-priority user message. A runtime hook injects it
  at the next LLM boundary and skips a not-yet-started tool call when needed.
- `follow_up(text)` queues a separate turn. `prompt()` automatically runs
  queued follow-ups in FIFO order after a completed turn.
- `prompt_once()` runs one turn without draining follow-ups.
- `continue_run(None)` runs the next queued steering or follow-up prompt.
- `cancel()` cancels the active handle and clears queued work. It returns
  `false` when no run is active.
- Calling `prompt()` while a run is active fails with `AlreadyRunning`; use
  `steer()` or `follow_up()` for concurrent input.

`subscribe()` returns a Tokio broadcast receiver of `InteractiveSessionEvent`.
Dropping the receiver unsubscribes it. Session lifecycle events and forwarded
`RunEvent` values carry the stable session id.

## Messages And Shared State

`messages()`, `shared_state()`, `latest_run()`, and `state()` return owned
snapshots, so callers never hold an internal lock. `replace_messages()` also
rewrites the backing `Session` and is only accepted while idle.

The facade injects caller-provided shared-state keys through
`RunConfig.initial_shared_state` before the first cycle, so runtime tools can
read them immediately. Each `AgentResult.shared_state` is then merged into the
latest host snapshot. Session metadata remains separate from mutable tool
state.

## Typed Final Output

When the agent's final output is JSON, deserialize it directly into a serde
type:

```rust,ignore
#[derive(serde::Deserialize)]
struct Report {
    summary: String,
    sources: Vec<String>,
}

let report: Report = result.deserialize()?;
```

Set the same type on the agent with `Agent::builder(...).output_type::<Report>()`
to make `Runner` validate successful final output before returning. Validation
errors are reported only after session persistence and terminal event emission
have completed.

`RunResult::deserialize<T>()` returns `FinalOutputError::Missing` when no final
output exists. Invalid JSON or a schema/type mismatch returns
`FinalOutputError::Deserialize` with the agent name, requested Rust type, and
the original `serde_json::Error`.

See `crates/vv-agent/examples/29_typed_final_output.rs` for a runnable example.
