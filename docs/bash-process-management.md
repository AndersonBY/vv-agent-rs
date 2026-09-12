# Bash Process Management

Contract 21 gives `bash` exactly six inputs: required `command`, optional
`exec_dir` and text `stdin`, `auto_confirm` (default false), `yield_time_ms`
and `timeout_seconds`.

| Parameter | Meaning | Range and default |
| --- | --- | --- |
| `yield_time_ms` | Initial observation wait; reaching it never kills the command | Integer 0..10000, default 1000 |
| `timeout_seconds` | Execution deadline from process start, including time after returning a handle | Optional integer 1..86400; omission means no deadline |

For example, start a local HTTP service with a one-minute execution limit:

```json
{"command":"python -m http.server 8000 --bind 127.0.0.1","yield_time_ms":1000,"timeout_seconds":60}
```

If it remains running, the completed management receipt is `SUCCESS` with
`continue`; process `status`, `session_id` and bounded output are in content,
with lightweight status/identity metadata. A checkpointed Runner can make its
next model call. Later process exit cannot overwrite that receipt or its digest.
Zero initial wait requests an immediate handle. Both
`check_background_command` and `stop_background_command` take only:

```json
{"session_id":"bg_0123456789ab"}
```

Query returns the current snapshot; stop requests bounded process-tree
termination. The deadline is set once at process start, including stdin
delivery, and the existing local watchdog enforces it without later queries.
Returning a handle, querying and subscribing never extend the deadline.

Ownership is the initiating task ID plus canonical local workspace root,
checked before process observation, output, artifacts or signals. Artifact
labels confer no access. The manager remains process-local memory. After
restart, `missing` does not claim that an external process exited.

Actual nonzero exits remain `ERROR`. Unix signals preserve their negative
signal code; no conventional shell exit code is invented. A timeout is an error
even when the child exits zero in response. Terminal stop requires confirmation
that no members of the managed process tree are executing. Unconfirmed
`stopping` or `unknown` observations contain no exit code and return
`SUCCESS` / `continue`. Confirmed terminal observations retain interactive
watcher notifications.

## Platform support and supervision

The complete local process-tree lifecycle in this release is supported on Linux.
A separate supervisor is created for each command before executing the original
command. The supervisor uses system calls and stack storage after fork, closes
inherited application descriptors, and reaps only its own descendants. The
embedding application never becomes a subreaper. No external Python installation
or additional shell backend is required.

`setsid()` and double-fork descendants remain managed after their original parent
exits. Only the supervisor's all-children-reaped proof can establish completion;
the original command's exit or signal is preserved. A lost supervisor without
that proof leaves an `unknown` session and its capture. The private control
channel belongs to `CapturedProcess.child` (`ManagedChild`), rather than a global
PID table. Drop of the owner's channel requests tree cleanup. Existing standard
`Command::spawn` execution-error reporting is retained; no additional blocking
startup handshake is added by this supervisor.

macOS and Windows retain command launch, stdin, output snapshots, and their
existing platform termination attempts, but no complete-tree proof is provided
there. Even a short command whose parent exited can remain `unknown`, and a
stop can remain `stopping`; neither is terminal success or contains an exit
code. This is an explicit availability limitation from requiring tree proof.
Use Linux when a workflow requires confirmed local completion or stop in this
release, and do not advertise the C21 local lifecycle as fully cross-platform.
Adopting a plain external `std::process::Child` is still accepted through
`Into<ManagedChild>`, but its parent status alone cannot prove the complete tree.

Live output uses a bounded Unicode head/tail preview and an immutable artifact
of the exact captured prefix. `read_file` recovers the complete artifact text.
Later output cannot change an earlier artifact; terminal queries reuse one
terminal artifact. Artifact errors never claim complete recovery succeeded.
Live and terminal capture-read failures can be retried by the same owner.
Recovery clears the output error without repeating completion notifications or
deleting a replacement file at a capture path that was already released.

`crates/vv-agent/tests/bash_process_management.rs` runs real registry producers
and SQLite checkpointed Runners with ScriptedLlmClient, real children and a
loopback HTTP service. It checks next model cycles, mixed batches and frozen
receipts as well as input boundaries, ownership, deadlines, stop and output.
Linux cases include detached/double-fork descendants, original parent signals,
unrelated-child and host-socket isolation, and closed application stdio. Session
unit tests cover read recovery and loss of supervisor confirmation.
Original shell/stdin/artifact coverage remains in `bash_tools.rs`.

```bash
cargo test --test bash_process_management --test bash_tools --test builtin_tool_behavior_contract
```

## Live output and deadlines

Live-output copying and artifact writes use a captured observation outside the
session state lock. A slow storage backend does not prevent the watchdog from
enforcing the command's original execution deadline. An observation whose
capture became unavailable during completion is reported as an output error;
it does not reset the deadline or authorize a duplicate command.
