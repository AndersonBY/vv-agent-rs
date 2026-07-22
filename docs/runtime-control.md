# Runtime Control And Resume

This document covers SDK behavior across `Agent`, `Runner`, `RunConfig`, live
handles, interrupted results, approvals, sessions, and runtime observers.

## Agent And Runner Defaults

`Agent.max_cycles`, `Agent.no_tool_policy`, and `Agent.tool_policy` are optional per-agent defaults.
Configured `Runner` values are reusable host defaults. The effective cycle
limit order is:

1. The per-run `RunConfig` value.
2. The configured `Runner` value.
3. The `Agent` value.
4. The framework default of `10`.

`max_cycles` accepts `1..=u32::MAX`. `max_handoffs` accepts
`0..=u32::MAX` and defaults to `10`. App Server turns use a separate default
of `80` cycles.

The no-tool policy order is per-run `RunConfig`, configured Runner default,
Agent, then `NoToolPolicy::Continue`. `Continue` preserves the tool-driven
default, `Finish` treats a normal assistant response as successful completion,
and `WaitUser` pauses on that response. These controls never inspect the text
or change which tools are available.

## Per-Run Controls

`RunConfig` can replace or extend these controls for one run:

- model reference, model provider, and model settings;
- workspace path/backend, session, initial messages, and shared state;
- cycle/handoff bounds, no-tool policy, tool policy, and a fresh tool registry factory;
- execution backend, cancellation, approval provider/broker/timeout;
- optional run-budget limits and a host-scoped cumulative cost meter; see
  `run-budgets.md`;
- runtime hooks, after-cycle lifecycle hooks, event store policy, trace sink,
  trace identity/workflow name,
  context providers, memory providers, and application state;
- before-cycle and interruption message providers plus a specific
  `SubTaskManager`;
- raw runtime log/stream observers, log preview length, and LLM request debug
  dumps.

```rust
use vv_agent::{Message, RunConfig, SubTaskManager};

let config = RunConfig::builder()
    .max_cycles(20)
    .before_cycle_messages(|cycle, _messages, _state| {
        vec![Message::user(format!("host context for cycle {cycle}"))]
    })
    .interruption_messages(|| Vec::new())
    .sub_task_manager(SubTaskManager::default())
    .runtime_log_handler(|event, _payload| {
        eprintln!("runtime event: {event}");
    })
    .trace_id("trace-host-request-42")
    .workflow_name("desktop-assistant")
    .log_preview_chars(200)
    .debug_dump_dir("./debug/llm")
    .build();
```

`tool_registry_factory` runs once per Agent execution, including handoff
targets. Tools registered through `ToolRegistry::register_tool` are
automatically planner-visible. A configured `debug_dump_dir` fails before the
first model call when the selected LLM client does not implement debug dumps;
the option is never silently ignored.

The shared capability manifest is
`crates/vv-agent/tests/fixtures/parity/run_config_controls.json`. Its Rust
end-to-end gate is `tests/run_config_controls.rs`.

## Language Adaptations

Python can specify a settings file, backend, builder callback, and timeout as
separate `RunConfig` fields. Rust packages the same per-run behavior in a
`ModelProvider`; `VvLlmModelProvider` is the settings-backed implementation.

Python also accepts a direct typed-event observer. Rust exposes independent
typed subscriptions from `RunHandle::events()`, so consumers do not need to
install a callback before starting the run. Raw runtime log and model stream
callbacks remain available in `RunConfig` on both sides.

Python `context` and Rust `app_state` are the equivalent typed host-application
payload. Python trace processors and Rust `TraceSink` provide the equivalent
trace export boundary.

Python's App Server also accepts a transport resume token plus an arbitrary
payload. That token belongs to the Python JSON-RPC bridge, not to the embedded
runner contract. Rust therefore exposes state-based `RunHandle::resume()` and
`RunHandle::resume_with_input()` instead of a token-shaped method that could
not be implemented by the runner itself.

## Live RunHandle Controls

`Runner::start()` returns a `RunHandle`. `cancel`, `approve`, `events`,
`result`, and state-based `resume` work on ordinary runner handles. `steer` and
`follow_up` require the handle to be the active handle of an
`InteractiveSession`; otherwise they return a deterministic unsupported error.
While attached, both methods call the same FIFO queues used by
`InteractiveSession::steer()` and `InteractiveSession::follow_up()`.

The attachment is generation-checked. Finishing, aborting, replacing, or
closing the active run detaches the controller before cancellation or session
cleanup, so a retained handle cannot keep the session alive or enqueue work
after close.

## Background Agent Tasks

`Agent::as_background_task()` returns a real background task facade. Starting
it delegates to `Runner::start()` and returns before completion. `poll()` and
`snapshot()` do not wait; `wait()` blocks until terminal state or timeout.

When a model starts the task, the child inherits the effective parent
capabilities: model provider, workspace/backend, cycle and handoff limits, tool
policy, approvals, execution backend, hooks, trace/event stores, context and
memory providers, application state, metadata, and registry/debug controls.
The child gets a fresh run identity and live shared-state snapshot. Parent
model selection/settings, session/history, before-cycle and interruption
providers, sub-task manager, raw runtime observers, and RunHandle event stream
are not reused. When the parent has a cancellation token, it is linked through
a child token: cancelling the parent cancels the child, while cancelling the
child does not cancel the parent.

## Interrupted Results

A run stopped by approval can be converted to `RunState`. Approving an
interruption and calling `Runner::resume()` executes the captured original tool
call once; it does not ask the model to recreate the call. A normal tool result
returns to the model loop, explicit wait/finish directives retain their normal
meaning, and `ToolUseBehavior` stop policies are applied after execution.
Approval resume receives a fresh run ID, so append-only event stores retain one
terminal event per run while returned event chains can still include the
interrupted predecessor. The equivalent live path is `RunHandle::approve()`
through an `ApprovalBroker`. Calling
`RunHandle::resume(state)` resolves the Runner and run configuration captured
by that state, rather than substituting the caller's current Runner defaults.

The fresh approval run remains inside the source trace. If the approved tool
returns `continue`, the new model loop receives the full configured
`max_cycles`; predecessor cycles do not reduce that budget. Supplying new input
for an approved tool call is rejected before cancellation projection or the
approval claim. With valid input, an already-cancelled resume emits one fresh
cancelled terminal without claiming the approval, executing the tool, or
running output guardrails. If approved terminal output fails typed-output
validation, its fresh terminal is persisted before the validation error is
returned to the caller.

## Tracing

`RunConfig::trace_id()` and `RunConfig::workflow_name()` are explicit tracing
controls. A non-empty per-run value wins over the configured Runner default. If
no trace ID is configured, Runner creates one. Arbitrary run metadata never
acts as trace configuration.

The workflow name is projected into run-span metadata. Every returned
`RunResult` also includes the ended canonical span object at
`metadata["run_span"]`, including `name`, trace/span IDs, timestamps, optional
parent ID, and metadata. Trace sink callbacks remain panic-isolated, unfinished
tool spans are closed as abandoned, and `flush()` still runs after the run and
agent spans close.

## Session Approvals

`ApprovalDecision::allow_session()` grants one tool for the lifetime of the
same broker. Reuse an explicit broker across run configs when the grant should
span multiple runs in one host session. `allow`, `deny`, and `timeout` do not
create a session grant.

## Session History

When `RunConfig.session` is set, Runner persists the complete current-turn
message delta: user input, assistant tool calls, and tool results. The next run
receives that complete history rather than a reconstructed user/final-answer
pair.

## Cancellation

`CancellationToken::cancel()` is idempotent. A callback registered after
cancellation runs once immediately. Once a terminal result is authoritative,
later cancellation cannot replace it. A cancelled result bypasses output
guardrails so guardrail side effects or messages cannot replace the recorded
cancellation reason.

## V1 Event Producers

Runner emits typed v1 events with canonical run, trace, agent, and session
identity. Raw runtime log and stream payloads are inputs to that producer, not
the stable product-facing API. Product code should consume `RunEventPayload` or
App Server item notifications instead of parsing raw log strings.
