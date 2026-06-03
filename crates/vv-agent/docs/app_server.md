# App Server

The App Server exposes `vv-agent` through bidirectional JSON-RPC over JSONL.
It is intended for desktop apps, browser extensions, and product shells that
need long-lived threads, live turn updates, approval dialogs, cancellation, and
replay without linking directly to runtime internals.

The App Server wraps `Runner::start()` and `RunHandle`. It does not replace the
`Agent`, `Runner`, `RunConfig`, `RunEvent`, or `RunEventStore` Rust APIs.

## CLI

Start the stdio server:

```bash
vv-agent app-server --listen stdio
```

Generate schemas:

```bash
vv-agent app-server generate-json-schema --out target/app-server-schema/json
vv-agent app-server generate-ts --out target/app-server-schema/typescript
```

Debug the protocol with a scripted model:

```bash
vv-agent debug app-server send-message "hello"
```

## Client Responsibilities

Clients must:

- Send `initialize` before every other request.
- Send the `initialized` notification before expecting async notifications.
- Keep reading notifications and server requests after `turn/start`.
- Answer server requests by the JSON-RPC request id.
- Treat stdout as protocol output only.
- Treat stderr as diagnostics and logs only.

## Protocol Lifecycle

Each JSON-RPC message is one line of JSON. The wire format intentionally omits
the `jsonrpc` field.

### Initialize

Client request:

```json
{"id":1,"method":"initialize","params":{"clientInfo":{"name":"v-claw","version":"0.1.0"},"capabilities":{"experimentalApi":false,"optOutNotificationMethods":[]}}}
```

Server response:

```json
{"id":1,"result":{"serverInfo":{"name":"vv-agent-rs","version":"0.4.0"},"protocolVersion":"2026-06-02","supportedTransports":["stdio"],"capabilities":{"thread":true,"turn":true,"itemStream":true,"approvalRequests":true,"eventReplay":true,"schemaExport":true}}}
```

Client readiness notification:

```json
{"method":"initialized"}
```

### Start A Thread

Client request:

```json
{"id":2,"method":"thread/start","params":{"title":"Investigate repo","model":"deepseek-v4-pro","ephemeral":false}}
```

Server response:

```json
{"id":2,"result":{"thread":{"id":"thread_1","title":"Investigate repo","model":"deepseek-v4-pro","status":"idle","archived":false,"ephemeral":false,"createdAtMs":1780410000000,"updatedAtMs":1780410000001}}}
```

Server notification:

```json
{"method":"thread/started","params":{"thread":{"id":"thread_1","title":"Investigate repo","model":"deepseek-v4-pro","status":"idle","archived":false,"ephemeral":false,"createdAtMs":1780410000000,"updatedAtMs":1780410000001}}}
```

### Start A Turn And Stream Items

Client request:

```json
{"id":3,"method":"turn/start","params":{"threadId":"thread_1","input":[{"text":"Summarize the workspace"}],"model":"deepseek-v4-pro"}}
```

Server response:

```json
{"id":3,"result":{"turn":{"id":"turn_1","threadId":"thread_1","runId":"assistant_run","status":"running","input":[{"text":"Summarize the workspace"}],"startedAtMs":1780410000100}}}
```

Example notifications:

```json
{"method":"turn/started","params":{"turn":{"id":"turn_1","threadId":"thread_1","runId":"assistant_run","status":"running","input":[{"text":"Summarize the workspace"}],"startedAtMs":1780410000100}}}
```

```json
{"method":"item/agentMessage/delta","params":{"threadId":"thread_1","turnId":"turn_1","itemId":"evt_1","delta":"The workspace contains..."}}
```

```json
{"method":"item/completed","params":{"threadId":"thread_1","turnId":"turn_1","item":{"id":"evt_2","runEventId":"evt_2","type":"toolCall","status":"completed","createdAtMs":1780410000200,"completedAtMs":1780410000200,"content":{"toolName":"task_finish","status":"success"}}}}
```

```json
{"method":"turn/completed","params":{"turn":{"id":"turn_1","threadId":"thread_1","runId":"assistant_run","status":"completed","input":[],"completedAtMs":1780410000300}}}
```

### Approval Request And Response

When a tool requires host approval, the server emits both a notification and a
JSON-RPC server request. The client must respond to the server request id.

Server notification:

```json
{"method":"approval/requested","params":{"threadId":"thread_1","turnId":"turn_1","requestId":"approval_1","toolName":"bash","preview":"Run cargo test","choices":["allow","deny"]}}
```

Server request:

```json
{"id":"approval_1","method":"approval/request","params":{"threadId":"thread_1","turnId":"turn_1","requestId":"approval_1","toolName":"bash","preview":"Run cargo test","choices":["allow","deny"]}}
```

Client response:

```json
{"id":"approval_1","result":{"decision":"allow"}}
```

Server notification:

```json
{"method":"approval/resolved","params":{"threadId":"thread_1","turnId":"turn_1","requestId":"approval_1","decision":"allow"}}
```

Clients may also resolve the same approval with `approval/resolve`:

```json
{"id":5,"method":"approval/resolve","params":{"threadId":"thread_1","turnId":"turn_1","requestId":"approval_1","decision":"allow"}}
```

### Replay Thread History

Client request:

```json
{"id":4,"method":"thread/read","params":{"threadId":"thread_1"}}
```

Server response:

```json
{"id":4,"result":{"thread":{"id":"thread_1","title":"Investigate repo","model":"deepseek-v4-pro","status":"idle","archived":false,"ephemeral":false,"createdAtMs":1780410000000,"updatedAtMs":1780410000300},"items":[{"id":"evt_1","runEventId":"evt_1","type":"agentMessage","status":"completed","createdAtMs":1780410000101,"completedAtMs":1780410000101,"content":{"text":"The workspace contains..."}}]}}
```

`thread/resume` returns the same replay items plus an `activeTurn` when a turn
is still running. It also subscribes the connection to future thread
notifications by default.

### Schema And Catalog Requests

`schema/export` returns both committed JSON Schema and TypeScript protocol
bundles:

```json
{"id":6,"method":"schema/export","params":{}}
```

`model/list` currently returns a valid, possibly empty model list. Product
clients should keep their existing model catalog fallback until a configured
App Server model catalog is added:

```json
{"id":7,"method":"model/list","params":{}}
```

`turn/steer` is reserved for follow-up input on active turns. Until runtime
steering is wired through this protocol method, the server returns an explicit
unsupported-method error with code `-32013`.

## Rust Test Client

`AppServerClient` is available for typed integration tests and future
consumers that want a Rust facade over the JSON-RPC protocol:

```rust
use vv_agent::app_server::client::AppServerClient;
```

The client keeps unmatched notifications in a backlog while waiting for typed
responses, so `next_message()` can still receive server requests and item
updates emitted around the same time as command responses.
