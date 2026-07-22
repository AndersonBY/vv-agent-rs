# App Server Runtime Mapping

## Runtime contracts

- `Runner::start(agent, input, run_config)` is the execution entrypoint for App Server turns.
- `RunHandle::events()` is the live runtime stream.
- `RunHandle::result()` is the terminal turn result.
- `RunHandle::cancel()`, `steer()`, `follow_up()`, `approve()`, and `state()` are the control surface.
- `RunEvent` is the versioned runtime event contract.
- `JsonlRunEventStore` is the baseline append-only event replay store.
- `InteractiveAgentClient` remains public, but App Server does not depend on product internals.

## App Server mapping

| App Server | Runtime |
| --- | --- |
| Thread | session metadata plus replay scope |
| Turn | one `RunHandle` |
| Item | `RunEvent` mapped to protocol item |
| Approval request | `ApprovalBroker` pending request routed through server request id |
| Tool-call delta | streamed `tool_call_progress` `RunEvent` metadata |
| Planned executor call | `tool_call_planned` is retained only as needed for approval argument routing; it emits and persists no App Server item |
| Started executor call | `tool_call_started` maps to `item/started` plus `item/toolCall/delta`; optional typed metadata becomes payload `toolMetadata` |
| Completed executor call | `tool_call_completed` maps to `item/completed`; its payload includes `directive`, `errorCode`, `executionStarted`, `durationMs`, and optional `toolMetadata` |
| Pre-execution denial | failed `item/completed` only, with no started item, `executionStarted=false`, and `durationMs=null` |
| Warning | failed run or event-stream exception |

## Constraints

- App Server must not parse raw runtime logs.
- App Server must not call `AgentRuntime` internals directly.
- App Server must not import product modules from v-claw or backend services.
- Planning must never be rendered or persisted as execution. A planned event
  may supply normalized arguments to a later approval request only.
- `toolMetadata` is a camelCase projection of declared `ToolMetadata`; generic
  tool metadata must not be promoted and `terminal=true` must not change turn
  state.
- Optional outcome fields are emitted only when the current source event
  contains them; mapping must not fabricate execution, duration, directive, or
  error facts.
- Item notifications are observations, not an exactly-once execution or
  durable-receipt protocol. Checkpoint v2 operation journals remain
  authoritative after an interrupted started call.
