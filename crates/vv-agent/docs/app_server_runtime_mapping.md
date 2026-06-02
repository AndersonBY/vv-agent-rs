# App Server Runtime Mapping

## Runtime contracts

- `RunHandle` provides live events, result waiting, cancellation, state reads, and approval resolution.
- `RunEvent` provides versioned event identity and typed payloads.
- `RunEventStore` provides append-only replay by `run_id`.
- `Runner::start()` is the only execution backend App Server should use for turns.

## App Server mapping

| App Server | Runtime |
| --- | --- |
| Thread | session metadata plus event replay scope |
| Turn | one `RunHandle` |
| Item | `RunEvent` mapped to protocol item |
| Approval request | `ApprovalBroker` pending request |

## Constraints

- App Server must not parse raw runtime logs.
- App Server must not call `AgentRuntime` internals directly.
- App Server must not own v-claw product behavior.
