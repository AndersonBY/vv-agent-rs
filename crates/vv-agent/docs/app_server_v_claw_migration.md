# v-claw App Server Migration Checklist

This checklist describes how v-claw should migrate from direct runtime
coupling to the `vv-agent-rs` App Server protocol.

## Host Boundary

- Implement `AppServerHost` in v-claw and keep browser, IM, local memory,
  notification, and release/update code in v-claw.
- Map each v-claw agent profile to `AgentResolutionRequest` and return a
  framework `Agent` from `resolve_agent()`.
- Build `RunConfig` with v-claw-owned tools, context providers, memory
  providers, workspace, execution backend, and approval policy.
- Implement `list_models()` from the same model settings source used to build
  runs, so `model/list` and actual execution cannot drift.

## Replacement Targets

| Current v-claw path | App Server replacement | Gate |
| --- | --- | --- |
| Parse runtime raw logs for timeline updates | Consume `item/*`, `turn/*`, and `approval/*` notifications | No task detail UI reads raw runtime log strings. |
| Local approval blocking around tool execution | Render `approval/request` server requests and answer by JSON-RPC id | Approval allow, deny, timeout, and disconnect cases pass through App Server. |
| Reconstruct task timeline from mixed task state | Use `thread/read` and `thread/resume` replay items | Reloading a task detail view renders from App Server items only. |
| Product-side session creation | Call `thread/start` | New task creation stores returned `threadId` as the product thread handle. |
| Active-turn input | Call `turn/steer` with `expectedTurnId` | Steering input is injected through the runtime hook instead of mutating memory directly. |
| Next-turn input | Call `turn/followUp` with `expectedTurnId` | Queued input starts a new turn only after the active turn completes. |
| Cancellation | Call `turn/interrupt` | Cancel button completes with an interrupted turn notification. |
| Model picker catalog | Call `model/list` through `AppServerHost::list_models()` | Product model choices come from the same host settings source used for execution. |

## Migration Gates

1. A single v-claw task can run through App Server under a feature flag.
2. The approval dialog resolves `approval/request` by JSON-RPC request id.
3. The task detail view renders only App Server `AppItem` records and live item notifications.
4. `thread/read` can restore a completed task after a v-claw restart.
5. `thread/resume` can attach to an active turn after reconnect and receive final notifications.
6. `turn/interrupt` is the only cancellation path under the feature flag.
7. Closing a task view calls `thread/unsubscribe` and reconciles `thread/closed`.
8. The direct runtime path remains available until two stable v-claw releases pass with App Server enabled.
9. Logs stay on stderr or product diagnostics; stdout remains protocol-only.

## Suggested Rollout

| Stage | Scope | Exit Criteria |
| --- | --- | --- |
| 1 | Hidden feature flag and local scripted task | One scripted task reaches `turn/completed` through stdio App Server. |
| 2 | Approval flow | v-claw approval dialog can allow and deny `approval/request` without local runtime blocking. |
| 3 | Timeline rendering | Task detail consumes `thread/read` and live notifications; raw runtime log parsing is disabled under the flag. |
| 4 | Resume and cancel | Active task reconnect and cancellation use `thread/resume` and `turn/interrupt`. |
| 5 | Default-on beta | App Server path is default for beta users; direct runtime path remains selectable. |
| 6 | Fallback removal review | Remove direct runtime coupling only after two stable releases with no blocker incidents. |

## Verification Matrix

| Behavior | Test |
| --- | --- |
| Task creation | `thread/start` response id is persisted in v-claw task state. |
| Turn streaming | `turn/started`, `item/agentMessage/delta`, `item/completed`, and `turn/completed` render in order. |
| Approval | Server request id matches `approval/requested.requestId`, and the response unblocks the run. |
| Replay | `thread/read` can render at least 100 items in stable order. |
| Reconnect | `thread/resume` returns active turn metadata and later completion notifications. |
| Cancel | `turn/interrupt` returns immediately and emits interrupted completion. |
| Steer and follow-up | `turn/steer` updates the active turn and `turn/followUp` starts exactly one subsequent turn. |
| Unsubscribe | `thread/unsubscribe` closes an idle thread after the final subscriber leaves. |
| Archive | `thread/archive` hides a task from default list without deleting replay items. |
