# App Server

The App Server exposes `vv-agent` through bidirectional JSON-RPC over JSONL.
It is intended for desktop apps, browser extensions, and product shells that
need long-lived threads, live turn updates, approval dialogs, cancellation, and
replay without linking directly to runtime internals.

The App Server wraps `Runner::start()` and `RunHandle`. It does not replace the
`Agent`, `Runner`, `RunConfig`, `RunEvent`, or `RunEventStore` Rust APIs.

## Host Provider

`AppServerHost` is the product integration boundary for agent and model
selection. A host resolves an `Agent`, builds a `RunConfig`, and lists models:

```rust
use std::sync::Arc;

use vv_agent::app_server::host::AppServerHost;
use vv_agent::app_server::processor::MessageProcessor;

let host: Arc<dyn AppServerHost> = Arc::new(product_host);
let (processor, outgoing) = MessageProcessor::with_host(
    128,
    runner,
    host,
    thread_store,
);
```

For every `turn/start`, the runtime reloads the persisted thread and calls both
`resolve_agent` and `build_run_config`. Their request values contain the
thread's `thread_id`, `agent_key`, `cwd`, and metadata, so tenant policy,
workspace selection, credentials, model routing, and tools can change between
turns without rebuilding the server. Resolution happens before a turn record
is created. A host error returns the Python v1 internal-error shape (`-32603`)
and leaves the thread without a newly created turn.

`MessageProcessor::with_runtime` and `AppServerRunAdapter::new` remain
convenience constructors for a fixed `Agent`; internally they use
`DefaultAppServerHost`. A production embedding or CLI that owns routing policy
should use `MessageProcessor::with_host`. `DefaultAppServerHost` can also be
configured with a fixed `RunConfig` and model list.

## CLI

Start the stdio server:

```bash
vv-agent app-server --listen stdio \
  --settings local_settings.json \
  --backend moonshot \
  --model kimi-k2.6 \
  --timeout-seconds 90
```

The four required production options may appear in any order and accept
`--key=value` syntax. `--timeout-seconds` is optional and defaults to 90
seconds. Settings and model resolution finish before the server reads stdin.
Production turns use a 30-second approval timeout.
The command runs the same generic `AppServer<StdioJsonlTransport>` used by
embedded integrations, so protocol errors, transport closure, overload, and
connection cleanup share one lifecycle implementation.

Generate schemas:

```bash
vv-agent app-server schema --out target/app-server-schema
vv-agent app-server generate-json-schema --out target/app-server-schema
vv-agent app-server generate-ts --out target/app-server-schema/typescript
```

`schema` and `generate-json-schema` are aliases.
Both JSON commands write individual schemas under `<out>/json/` plus
`<out>/json/vv_agent_app_server.schemas.json`. Request params such as
`turn/start` are represented in `ClientRequest.json`; there is no separate
`TurnStartParams.json` file.

Debug the protocol with a scripted model:

```bash
vv-agent debug app-server send-message "hello"
```

## Client Responsibilities

Clients must:

- Send `initialize` before every other request.
- Optionally send the `initialized` notification to complete the shared client
  handshake. Python v1 compatibility allows async notifications immediately
  after the initialize response.
- Keep reading notifications and server requests after `turn/start`.
- Answer server requests by the JSON-RPC request id.
- Treat stdout as protocol output only.
- Treat stderr as diagnostics and logs only.

## Protocol Lifecycle

Each JSON-RPC message is one line of JSON. Every request, response,
notification, and error includes `"jsonrpc": "2.0"`.

### Initialize

Client request:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"v-claw","version":"0.1.0"},"capabilities":{"experimentalApi":false,"optOutNotificationMethods":[]}}}
```

Server response:

```json
{"jsonrpc":"2.0","id":1,"result":{"userAgent":"vv-agent-app-server","protocolVersion":"v1","capabilities":{"modelList":true,"threadLifecycle":true,"notificationOptOut":true,"schemaExport":true,"approvalResolve":true}}}
```

`threadLifecycle` is `false` when the processor has no runtime adapter. A host
must not advertise thread and turn support that it cannot execute.

Client readiness notification:

```json
{"jsonrpc":"2.0","method":"initialized"}
```

### Start A Thread

Client request:

```json
{"id":2,"method":"thread/start","params":{"agentKey":"default","cwd":"./workspace","metadata":{"title":"Investigate repo"}}}
```

Server response:

```json
{"id":2,"result":{"threadId":"thread_1","agentKey":"default","cwd":"./workspace","status":"idle"}}
```

Server notification:

```json
{"method":"thread/started","params":{"threadId":"thread_1","agentKey":"default","cwd":"./workspace","status":"idle"}}
```

`thread/unsubscribe` removes the current connection from the thread. When no
subscriber or active turn remains, it returns
`{"threadId":"thread_1","subscribed":false,"closed":true}` and emits
`thread/closed` plus `thread/status/changed`.

### Start A Turn And Stream Items

Client request:

```json
{"id":3,"method":"turn/start","params":{"threadId":"thread_1","input":[{"type":"text","text":"Summarize the workspace"}]}}
```

Server response:

```json
{"id":3,"result":{"threadId":"thread_1","turnId":"turn_1","status":"running"}}
```

Example notifications:

```json
{"method":"turn/started","params":{"threadId":"thread_1","turnId":"turn_1"}}
```

```json
{"method":"item/agentMessage/delta","params":{"itemId":"evt_1","threadId":"thread_1","turnId":"turn_1","type":"agentMessage","status":"inProgress","payload":{"text":"The workspace contains..."},"createdAt":1780410000101,"updatedAt":1780410000101,"delta":"The workspace contains..."}}
```

```json
{"method":"item/completed","params":{"itemId":"evt_2","threadId":"thread_1","turnId":"turn_1","type":"toolCall","status":"completed","payload":{"toolCallId":"finish","toolName":"task_finish","status":"success","directive":"finish","errorCode":null,"executionStarted":true,"durationMs":3},"createdAt":1780410000200,"updatedAt":1780410000200}}
```

```json
{"method":"turn/completed","params":{"threadId":"thread_1","turnId":"turn_1","runId":"assistant_run","status":"completed","finalOutput":"The workspace contains...","tokenUsage":{"prompt_tokens":10,"completion_tokens":4,"total_tokens":14,"cached_tokens":0,"reasoning_tokens":0,"input_tokens":10,"output_tokens":4,"cache_creation_tokens":0,"cache_usage":{"status":"provider_reported","read_tokens":0,"write_tokens":null,"uncached_input_tokens":10,"source":"provider_usage"}}}}
```

Terminal notifications preserve `finalOutput`, `error`, `tokenUsage`,
`budgetUsage`, and `budgetExhaustion` when those values are available. The same
objects are stored in the turn result for replay. `cache_usage.read_tokens: 0`
is an observed zero; `null` with `status: "accounting_missing"` means the
provider did not supply enough cache accounting and must not be reported as a
zero-percent hit.

Use `turn/steer` to add input to the active turn and `turn/followUp` to queue a
new turn after it completes. Both take `threadId`, `expectedTurnId`, and
`input`, and return `{"threadId":"thread_1","turnId":"turn_1","queued":true}`.
`turn/interrupt` uses the same `expectedTurnId` guard. Missing active turns
return `-32030`; stale turn ids return `-32031`.

### Tool Lifecycle Projection

The runtime and App Server intentionally expose different views of planning:

| Runtime `RunEvent` | App Server projection |
| --- | --- |
| `tool_call_planned` | No notification and no persisted `AppItem`. The adapter may retain normalized arguments internally for a later approval request, but planning is never presented as execution. |
| `tool_call_started` | Existing `item/started` and `item/toolCall/delta` notifications. The item payload adds `toolMetadata` when the tool declared typed metadata. |
| `tool_call_completed` | Existing `item/completed` notification. The payload includes `directive`, `errorCode`, `executionStarted`, and `durationMs` when those fields exist on the source event, plus optional `toolMetadata`. |

App Server uses camelCase inside `AppItem.payload`. A typed declaration is:

```json
{
  "toolMetadata": {
    "sideEffect": "read",
    "idempotency": "supported",
    "terminal": false,
    "capabilityTags": ["source.inspect"],
    "costDimensions": ["workspace.bytes_read"]
  }
}
```

`terminal` is a capability declaration only; App Server derives turn state
from the existing runtime result and directive, not from this flag. Capability
tags and cost dimensions remain opaque exact-match host labels.

A policy or approval denial that never crosses the started boundary produces
only a failed `item/completed` tool item with `executionStarted=false`,
`durationMs=null`, and the runtime error code such as `tool_not_allowed`. It
does not produce `item/started`. Conversely, process loss after a started event
may leave no completed notification; clients must not infer exactly-once tool
execution from the item stream.

The App Server protocol stays at `v1`, and all fields above are additive.
Older clients may ignore them. When replay maps a legacy completed `RunEvent`
that did not contain the additive outcome fields, it keeps the legacy payload
instead of fabricating `directive`, `errorCode`, `executionStarted`, or
`durationMs`. Omitting typed metadata also omits `toolMetadata`; generic tool
metadata is never promoted into this field.

### Approval Request And Response

When a tool requires host approval, the server emits both a notification and a
JSON-RPC server request. The client must respond to the server request id.

Server notification:

```json
{"method":"approval/requested","params":{"requestId":"approval_1","threadId":"thread_1","turnId":"turn_1","toolCallId":"call_1","toolName":"bash","preview":"Run cargo test","arguments":{"cmd":"cargo test"}}}
```

Server request:

```json
{"id":"approval_1","method":"approval/request","params":{"requestId":"approval_1","threadId":"thread_1","turnId":"turn_1","toolCallId":"call_1","toolName":"bash","preview":"Run cargo test","arguments":{"cmd":"cargo test"}}}
```

Client response:

```json
{"id":"approval_1","result":{"decision":"allow_session"}}
```

Server notification:

```json
{"method":"approval/resolved","params":{"threadId":"thread_1","turnId":"turn_1","requestId":"approval_1","decision":"allow_session"}}
```

Clients may also resolve the same approval with `approval/resolve`:

```json
{"id":5,"method":"approval/resolve","params":{"threadId":"thread_1","turnId":"turn_1","requestId":"approval_1","decision":"allow_session"}}
```

Normal response envelopes and `approval/resolve` accept the canonical decisions
`allow`, `allow_session`, `deny`, and `timeout`. The
`approval/resolved` notification preserves the client's decision;
`allow_session` is not collapsed to `allow`.

### Replay Thread History

Client request:

```json
{"id":4,"method":"thread/read","params":{"threadId":"thread_1"}}
```

Server response:

```json
{"id":4,"result":{"thread":{"threadId":"thread_1","agentKey":"default","cwd":"./workspace","createdAt":1780410000000,"updatedAt":1780410000300,"status":"idle","metadata":{"title":"Investigate repo"}},"turns":[{"turnId":"turn_1","threadId":"thread_1","runId":"assistant_run","status":"completed","startedAt":1780410000100,"completedAt":1780410000300,"input":[{"type":"text","text":"Summarize the workspace"}],"result":{"finalOutput":"The workspace contains..."}}],"items":[{"itemId":"evt_1","threadId":"thread_1","turnId":"turn_1","type":"agentMessage","status":"completed","payload":{"text":"The workspace contains..."},"createdAt":1780410000101,"updatedAt":1780410000101}]}}
```

`thread/resume` returns the same thread, turns, and replay items. It subscribes
the connection to future thread notifications by default.

### Schema And Catalog Requests

`schema/export` returns both committed JSON Schema and TypeScript protocol
bundles:

```json
{"id":6,"method":"schema/export","params":{}}
```

`model/list` delegates to the injected `AppServerHost`. The default host returns
its configured catalog, which may be empty:

```json
{"id":7,"method":"model/list","params":{}}
```

```json
{"id":7,"result":{"models":[{"id":"kimi-k2.6","provider":"moonshot","displayName":"Kimi K2.6","contextLength":262144,"supportsTools":true}]}}
```

Malformed JSON produces a `-32700` protocol error with `id: null`; stdio keeps
reading and continues to flush asynchronous outgoing messages without waiting
for another input line.

## Rust Test Client

`AppServerClient` is available for typed integration tests and future
consumers that want a Rust facade over the JSON-RPC protocol:

```rust
use vv_agent::app_server::client::AppServerClient;
```

The client keeps unmatched notifications in a backlog while waiting for typed
responses, so `next_message()` can still receive server requests and item
updates emitted around the same time as command responses.
