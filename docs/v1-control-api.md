# v1 control API

This document specifies the control protocol between the `cowfs` CLI (and other local tools such as the treehouse companion) and the cowfs daemon.
It implements the "CLI on top of a Unix-socket control API" decision in `docs/design.md`.
The reference implementation is `crates/cowfs-ctl` (types, framing, client, server framework, `StubHandler`).
The CLI is `crates/cowfs-cli`.
Issue: #13.

## Goals and non-goals

- One small, versioned, line-oriented protocol that a shell script, the CLI and a Go program (treehouse) can all speak.
- Long operations stream progress and can be cancelled by the client.
- Errors are structured and their codes are stable, so callers can branch on them.
- The protocol describes operations.
  It does not implement them.
  The daemon implements the `ControlHandler` trait from `cowfs-ctl`.
- Single-user model.
  There is no authentication beyond filesystem permissions and a peer uid check.
- Out of v1: remote access, TLS, multiple users, a Windows transport.

## Transport

- A Unix domain stream socket.
- Default location: `$XDG_RUNTIME_DIR/cowfs/control.sock` when `XDG_RUNTIME_DIR` is set and absolute, otherwise `<temp dir>/cowfs-<uid>/control.sock`.
  On macOS the temp dir is the per-user `$TMPDIR`.
- Override with `--socket PATH` or the `COWFS_SOCKET` environment variable.
- Unix socket paths are limited to about 100 bytes.
  A longer path fails at bind and connect with a clear error.

### Permissions

- The directory holding the socket must be owned by the current uid and have no group or other permission bits (mode `0700`).
  The server creates it with mode `0700` if it is missing.
  It refuses to use an existing directory that is owned by someone else or is accessible to group or other.
  Because of this, `--socket /tmp/x.sock` is refused: put the socket in a private directory.
- The socket file is set to mode `0600` after bind.
  The directory permissions close the window between bind and chmod.
- After accept, the server reads the peer uid from the kernel (`SO_PEERCRED` on Linux, `getpeereid` on macOS).
  A peer whose uid differs from the server's, or whose uid cannot be read, is sent a `permission_denied` error and disconnected before any frame is read (fail closed).
  The check is a `PeerCheck` seam in `ServerOptions`, so tests can force both failure paths.
- This is the only unsafe code in the two crates.
  It is one audited module, `cowfs-ctl/src/sys.rs`, with `getuid` and the peer credential call.

### Single instance and stale sockets

- The server takes an exclusive advisory lock on `<socket>.lock` for its whole lifetime.
  A second server on the same socket fails to start.
  The lock file is never deleted, because deleting lock files races with new lockers.
- While holding the lock, if the socket path exists:
  - if it is not a socket, startup fails and nothing is removed;
  - if a connect succeeds, a live server that does not use the lock owns it, and startup fails without touching it;
  - if the connect is refused, the socket is stale (a crashed server) and is removed.
- The socket file is removed on graceful shutdown.

## Framing

- JSON lines: one JSON object per line, UTF-8, terminated by `\n`.
- Line length is bounded.
  The server accepts at most 1 MiB per line (`MAX_REQUEST_LINE`).
  The client accepts at most 64 MiB per line (`MAX_RESPONSE_LINE`).
  A longer line from a client gets a `line_too_long` error and the connection is closed, because the stream cannot be resynchronised cheaply.
- Lines have time limits too, see "Limits and timeouts".
- A final line without a terminating `\n` (a truncated write followed by EOF) is ignored.
- Malformed JSON, a non-object frame, or a missing field gets a `malformed_frame` error frame.
  The connection stays open.
- Receivers ignore unknown fields in every object.

## Frames

Every frame has a `type`.

### Client to server

| `type` | Fields | Meaning |
|---|---|---|
| `hello` | `versions` (array of integers), `client` (string, optional) | First frame. Lists the protocol versions the client speaks. |
| `request` | `id` (unsigned integer), `method`, `params` (object, optional, defaults to `{}`) | Start an operation. |
| `cancel` | `id` | Ask the server to cancel the in-flight request with that id. |

### Server to client

| `type` | Fields | Meaning |
|---|---|---|
| `hello` | `version` (integer), `server` (string), `methods` (array of strings) | Handshake reply. `methods` lists every method the server supports. |
| `progress` | `id`, `event` | Zero or more per streaming request, before the final frame. |
| `response` | `id`, `result` | Final success frame. `result` is `{"kind": ..., "data": {...}}`. |
| `error` | `id` (integer or null), `error` | Final failure frame for a request, or a connection-level error when `id` is null. |

Exactly one `response` or `error` frame ends each request.

## Handshake and versions

1. The client connects and sends `hello` with every protocol version it speaks, for example `{"type":"hello","versions":[1],"client":"cowfs-cli/0.0.0"}`.
2. The server picks the highest version both sides speak and replies with `hello`.
3. If there is no common version, the server sends `error` with code `unsupported_version` and `details: {"supported": [1]}`, then closes.
4. Any frame other than `hello` before the handshake gets `handshake_required` and a close.
   The whole handshake must complete within the handshake timeout (10 seconds by default), counted from accept and not per read, so a client that drips one byte at a time is dropped too.
   The server sends `timeout` and closes.

The version is a major version.
It is `1` for everything in this document.

## Requests and responses

Request ids are chosen by the client, unique among its in-flight requests on that connection.
A duplicate in-flight id gets `duplicate_id`.
A connection may have up to 32 requests in flight, and the server up to 64 across all connections, more get `busy`.
Responses to concurrent requests can arrive in any order and are matched by `id`.

Strings that name snapshots must be valid path components: non-empty, at most 255 bytes, no `/`, no NUL, not `.` or `..`.
A violation is `invalid_params`.

| `method` | `params` | `result.kind` and `result.data` | Streams progress |
|---|---|---|---|
| `ping` | `{}` | `pong`: `{}` | no |
| `version` | `{}` | `version`: `{protocol, server, ctl}` | no |
| `status` | `{}` | `status`: `{store_path, mount_path, snapshot_count, block_count, logical_bytes, stored_bytes, uptime_secs}` | no |
| `snapshot_list` | `{}` | `snapshot_list`: `{snapshots: [SnapshotInfo]}` | no |
| `snapshot_create` | `{name, from?}` | `snapshot`: `SnapshotInfo` | no |
| `snapshot_rm` | `{name, expect_no_holders?}` | `ok`: `{}` | no |
| `snapshot_reset` | `{name, from, expect_no_holders?}` | `snapshot`: `SnapshotInfo` | no |
| `snapshot_rename` | `{from, to}` | `snapshot`: `SnapshotInfo` | no |
| `snapshot_promote` | `{name}` | `snapshot`: `SnapshotInfo` | no |
| `gc` | `{dry_run}` (required) | `gc`: `GcReport` | yes |
| `fsck` | `{}` | `fsck`: `FsckReport` | yes |
| `import` | `{path, name}` | `import`: `ImportReport` | yes |
| `base_refresh` | `{repo, git_ref, name?}` | `base_refresh`: `BaseRefreshReport` | yes |
| `ps` | `{snapshot}` | `processes`: `{processes: [ProcessInfo]}` | no |
| `mount_info` | `{}` | `mount_info`: `{mount_path, adapter, mounted}` | no |
| `shutdown` | `{}` (no fields) | `ok`: `{}` | no |

### Params strictness

- `params` must be a JSON object (or absent, which means `{}`).
  An array, string or number is `invalid_params`.
- Params of the destructive methods `gc`, `snapshot_rm`, `snapshot_reset`, `import`, `base_refresh` and `shutdown` reject unknown fields with `invalid_params`.
  A misspelt option must never turn into a different operation.
  Params of the read-only methods and of `snapshot_create`, `snapshot_rename`, `snapshot_promote` and `ps` ignore unknown fields.
- `gc` requires an explicit `dry_run` boolean.
  There is no default, so a real run is never the result of an omission or a typo.
- Consequence for evolution: a new optional field on a strict method is not compatible within a major version.
  It needs a new method, a new major version or a capability listed in `hello.methods`.

### Validation done by the framework

The framework validates before it calls the handler, so a handler never sees these values.
The functions are public in `cowfs-ctl` (`validate_snapshot_name`, `name_key`, `validate_repo`, `validate_git_ref`, `validate_abs_path`).

- Snapshot names (`validate_snapshot_name`): non-empty, at most 255 bytes, valid UTF-8 (JSON guarantees it), no `/`, no control characters (NUL, newline, tab and ESC included), and no leading `.`.
  The leading dot rule covers `.`, `..`, AppleDouble `._*` and NFS `.nfs*`.
- Name collisions: a backend must refuse `snapshot_create`, `snapshot_rename`, `import` and `base_refresh` when the new name has the same `name_key` as another snapshot.
  The key is NFC, lowercased, then NFC again, so `Foo` and `foo`, or a precomposed and a decomposed `cafe` collide.
  A rename that only changes the case of the same snapshot is allowed.
  The collision is `already_exists`.
  Folding can grow a name past 255 bytes, so a key longer than the bound becomes `#` plus a BLAKE3
  hash of the folded form: the key is always a valid snapshot name, at the cost of a theoretical
  hash collision.
  This function is the API-level contract and is kept small so it can be reconciled with the naming rules of `cowfs-core`.
- `repo` and `import.path`: absolute, at most 4096 bytes, no control characters.
  An absolute path can never be read as an option by a tool it is passed to.
- `git_ref`: at most 255 bytes, not empty, no leading `-`, no control characters or whitespace, none of `~ ^ : ? * [ \`, no `..`, `@{`, trailing `/` or trailing `.lock`.
  `--upload-pack=x` is rejected.
- Paths on the wire are UTF-8 strings.
  The CLI refuses a path that is not UTF-8 with exit code 2 and a message.
  Carrying arbitrary bytes is left to a later protocol version.
- Human output escapes control characters in every string it prints.

### Busy

A snapshot is busy when a holder exists:
a process with its working directory, an open file descriptor or a lock inside `<mount_path>/<name>`, as `ps` reports them, or an open handle the mount adapter has on an inode of that snapshot.
A real backend must count both, because on an NFS loopback or FUSE mount the adapter sees handles and a process scan sees the rest.
`busy` carries `details: {"holders": [ProcessInfo]}` when the backend knows them.

`ps` then `snapshot_rm` is racy by nature: a process can start between the two calls.
So the authoritative check is inside the operation, and the framework owns the lock that makes it
atomic:

- The framework keeps one lock per snapshot name.
- For `snapshot_rm` and `snapshot_reset` it builds a `HolderGuard` and runs `check_holders` itself,
  so a holder present when the request arrives is refused before the handler is called.
- It then calls the handler's `remove(name, &guard)` or `swap(name, from, &guard)`.
  A handler must hold `guard.lock()` across its holder check and its change, and whoever adds or
  removes a holder must take the same lock.
  A handler that ignores this is not conformant: `cowfs_ctl::handler_conformance` fails it, and it
  must pass before a daemon is wired in.
- `expect_no_holders: false` skips the check, and is what `cowfs snapshot rm --force` sends.

The result is either done, or `busy` with nothing changed.
`busy` is checked before `not_found`, so a snapshot with a holder reports `busy` even when the
source snapshot is also missing.
The cost of `ps` and of the holder check is a process scan, bounded by the request timeouts.
There is no bounded "wait until free" call in v1: a caller that gets `busy` retries.
`cowfs_ctl::handler_conformance(handler, add_holder)` is the reusable check a backend must pass:
it verifies that a holder makes both operations `busy` and change nothing, that `holders` reports
injected holders, that the swap is atomic and leaves exactly one snapshot, and that concurrent
changes of one snapshot are serialised.
`add_holder` must add the holder while holding the same lock `guard.lock()` returns, which is what
a real adapter must do.

### `snapshot_reset`

Replaces snapshot `name` with a fresh O(1) clone of `from`, in one step under one lock.
There is no instant at which `name` is missing, and a crash leaves either the old or the new snapshot under that name.
It fails `not_found` when either snapshot is missing, `invalid_params` when they are the same, and `busy` as described above.
The result is the new `SnapshotInfo`, with `parent` set to `from`.
Treehouse mode (b) uses it to reset a slot to the warm base.

### Semantics worth knowing

- `snapshot_create` without `from` creates a snapshot of the empty tree.
  With `from` it is an O(1) writable clone of that snapshot.
  It fails with `already_exists` if `name` is taken and `not_found` if `from` is missing.
- `snapshot_rm` fails with `busy` if the snapshot has a holder (see "Busy") and `not_found` if it does not exist.
- `snapshot_promote` turns a clone into a base: `base` becomes non-null with all-null fields.
  It is idempotent.
- `base_refresh` builds or refreshes the warm base snapshot for a repository at a git ref.
  If `name` is omitted the daemon derives a stable name from `repo`.
  The previous base with the same name is replaced only after the new one is complete.
  The result names the snapshot and the previous commit, if any.
- `gc` with `dry_run: true` frees nothing and reports what would be freed.
  Garbage collection is mark and sweep from snapshot roots, as in `docs/design.md`.
- `import` follows the migration rules in `docs/design.md`: ingest, re-read the source, verify by hash, and only then report success.
  The daemon never swaps a directory for a mount on its own: the caller does that after `import` succeeds and `verified` is true.
  The report carries the hash algorithm (`blake3`), the root hash of the source tree and the root hash of the imported tree, so the caller can hash the source itself and compare before it swaps.
  What is hashed: entry names, kind, permission bits (`mode & 0o7777`), file content and symlink targets.
  Not hashed: ownership, timestamps, xattrs and hard link identity.
  Each directory hashes its entries sorted by name bytes: for each entry the name length and bytes, the mode, a kind byte, and then the file size and content hash, the symlink target or the child directory hash.
  Symlinks are never followed.
  `cowfs_ctl::hash_tree` implements it.
  `verified` is true only when the two root hashes are equal and `mismatches` is empty.
- `ps` lists processes that hold the snapshot's directory on the mount: as a working directory, an open file or a lock.
  Treehouse detects only working directories (`docs/spikes/5-treehouse-process-detection.md`), so this call closes issue #20.
- `shutdown` responds first, then cancels other in-flight requests, closes connections, removes the socket and stops.

### Types

`SnapshotInfo`:

- `name`: string.
- `parent`: string or null, the snapshot it was cloned from.
- `base`: null, or `{repo, git_ref, commit}` where each field is a string or null.
- `created_unix_ms`: integer.

`GcReport`: `{dry_run, candidate_blocks, candidate_bytes, freed_blocks, freed_bytes}`.

`FsckReport`: `{ok, blocks_checked, bytes_checked, snapshots_checked, problems: [{kind, detail}]}`.

`ImportReport`: `{name, files, bytes, verified, hash_algorithm, source_root_hash, imported_root_hash, mismatches: [{path, reason}], mismatches_truncated}`.
`mismatches` lists at most 100 differences, relative to the source root, and `mismatches_truncated` says whether there were more.

`BaseRefreshReport`: `{snapshot: SnapshotInfo, previous_commit}`.

`ProcessInfo`: `{pid, command, holds: [{kind, path}]}`, where `kind` is `cwd`, `fd` or `lock`.

### Progress events

```json
{"type":"progress","id":7,"event":{"phase":"mark","done":120,"total":400,"unit":"items","message":null}}
```

- `phase` is a short lowercase word naming the current step.
- `done` and `total` count `unit`, which is `bytes` or `items`.
  `total` is null when unknown.
- `message` is optional free text.
- Progress is advisory and lossy in meaning: clients must not depend on the exact events or their count.
- The daemon should emit at most about 20 events per second.

## Cancellation

- The client sends `cancel` with the request id.
  The server cancels that request's token.
  The handler stops as soon as it can and returns an error with code `cancelled`.
  The request still ends with exactly one final frame, which is `error` with `cancelled` unless the operation finished first.
- A `cancel` for an unknown or finished id is ignored.
- Closing the connection cancels every request in flight on it.
- Half-close is not a cancel.
  A client that sends its requests and then shuts down its write side means "no more requests": the server finishes the requests in flight, sends their final frames, then closes.
  So `printf '{...}\n{...}\n' | nc -U SOCK` style scripts work.
  A client that is really gone shows up as a failed write, which cancels its requests.
- A client that stops reading is treated the same way.
  The server sets a write timeout (30 seconds).
  When a write times out or fails, the connection is closed and its requests are cancelled.
- Every request that is still in flight when the server shuts down ends with a final `error` frame, code `shutting_down`, before the connection is closed.
- A cancelled operation leaves the store consistent.
  Handlers must make cancellation safe at every point where they check the token.

## Errors

```json
{"type":"error","id":3,"error":{"code":"not_found","message":"snapshot \"a\" does not exist","details":null}}
```

`code` is a stable snake_case string.
`message` is for humans and may change.
`details` is optional structured data.

| Code | Meaning |
|---|---|
| `malformed_frame` | The line was not valid JSON or a required field was missing or of the wrong type. |
| `line_too_long` | The line exceeded the limit. The connection is closed. |
| `handshake_required` | A frame arrived before `hello`. |
| `unsupported_version` | No common protocol version. |
| `permission_denied` | The peer uid does not match the server. |
| `unknown_frame` | The frame `type` is unknown. |
| `unknown_method` | The `method` is unknown. The connection stays open. |
| `invalid_params` | The params are missing, of the wrong type, or fail validation. |
| `duplicate_id` | The id is already in flight. |
| `busy` | Too many requests in flight, or the target is in use. |
| `not_found` | The named snapshot, repo or path does not exist. |
| `already_exists` | The name is taken. |
| `unsupported` | The backend does not implement the operation. |
| `cancelled` | The request was cancelled. |
| `shutting_down` | The server is stopping. |
| `io_error` | An I/O error in the backend. |
| `too_many_connections` | The server is at its connection cap. It is the only frame sent, and the connection is closed. |
| `timeout` | The handshake, a request line or an idle connection exceeded its deadline. The connection is closed. |
| `internal` | A bug, including a panic in a handler. The connection stays open. |

Clients must treat an unknown code as a generic failure and show `message`.

## Limits and timeouts

Server defaults, all in `ServerOptions`:

| Limit | Default | On violation |
|---|---|---|
| Connections served at once | 64 | The least recently active idle connection is evicted and the new one admitted; if every connection has a request in flight, the new connection gets `too_many_connections` and is closed. |
| Idle for eviction | 10 s | A connection with nothing in flight and no traffic for this long may be evicted to make room. A connection with a request in flight is never evicted. |
| Requests in flight per connection | 32 | `busy` for the request. |
| Requests in flight, all connections | 64 | `busy` for the request. |
| Handshake, total from accept | 10 s | `timeout`, close. Not extended by partial data. |
| A request line, first byte to newline | 10 s | `timeout`, close. This stops a client that sends one byte every few seconds. |
| Idle connection (nothing in flight, no request) | 30 s | `timeout`, close. A connection with a request in flight is never idle. |
| Write to a client | 30 s | Close, cancel the connection's requests. |
| Request line size | 1 MiB | `line_too_long`, close. |

The server uses one thread per connection and one per request, so the caps above bound the thread count: at most 64 connection threads plus 64 request threads.
The flood test opens 3000 connections and checks the thread count stays under the cap, and a second test fills the cap with idle connections and checks a legitimate client is still served within the eviction window.
The bounds are not only about resources: a connection that is idle, or accepted while the server is stopping, gets a structured answer rather than silence.

Client defaults, `ClientOptions`, and the CLI `--timeout`:

- Connect: 5 s, and a connect that never completes is exit 4 like any other timeout.
  A refusal is retried briefly, because a full listen backlog reports the same error as no listener.
- Handshake: 5 s in total.
- Response: no frame of any kind for 30 s (`--timeout`, `COWFS_TIMEOUT`).
  Every progress frame starts the clock again, so a long `gc` that reports progress is never cut off, and a wedged daemon is noticed in 30 s.
- A timeout is `ClientError::Timeout`, and the CLI exits 4.

## Shutdown

- A `shutdown` request, `ShutdownHandle::shutdown` or a signal in `cowfs serve` begins shutdown.
- First, the accept queue is drained: every connection that connected but was not accepted yet is
  answered with its `hello` and a `shutting_down` error per request, then closed politely, so a
  `cowfs` call in flight at shutdown never sees a bare EOF.
  Closing a queued connection without answering makes the kernel send RST, which is why this exists.
- Then the socket file is removed and the listener is closed, so new clients get "no daemon" at once
  (exit 3) instead of a connection nobody serves.
- Every close after an error reply follows the same discipline: write the frame, half-close, read
  away what the peer already sent, then close.
- In-flight requests are cancelled.
  Handlers that stop send their final frame, which is `shutting_down` (a `cancelled` result is rewritten to it).
- The server waits up to the shutdown deadline (5 s by default).
  Handlers still running after it are abandoned: their requests get `shutting_down`, their connections are closed and the server returns.
  The handler threads are detached and die with the process.
- In `cowfs serve` the first SIGINT or SIGTERM begins shutdown and a second one exits the process at once with code 130.

## Evolution rules

Within major version 1:

1. Adding an optional field, with a default, to a response, an event or a frame is compatible.
   Receivers ignore fields they do not know, and senders omit optional fields at their default.
   Request params of the destructive methods are strict (see "Params strictness"): a new field there is not compatible without a new method or version.
2. Adding a method, a response `kind`, a frame `type`, an error code or a progress `phase` is compatible.
   A client learns which methods exist from `hello.methods`.
   A response with a `kind` the client does not know decodes to `Response::Unknown { kind, data }` and the CLI prints its raw data; it never breaks the client.
3. Adding a value to an enum-typed field (`unit`, hold `kind`) is compatible: clients map unknown values to `other`.
4. Removing or renaming a field, method or code, changing a type, or changing the meaning of an existing field is breaking.
   It requires a new major version, and the server may speak both during a transition.
5. A change that alters the wire format for any existing message fails the golden-file test in `crates/cowfs-ctl/tests/wire.rs`.
   Updating the golden file (`COWFS_UPDATE_GOLDEN=1`) is how a reviewer sees a protocol change.

## Server framework

- `ControlHandler` (`cowfs-ctl`) is the trait a daemon implements: `status`, `snapshot_list`, `snapshot_create`, `remove`, `swap`, `holders`, `gc`, `fsck`, `import`, `base_refresh`, `mount_info`, `shutdown`.
  `ping`, `version` and `ps` (which is `holders`) are answered by the framework.
- `remove` and `swap` replace the old `snapshot_rm` and `snapshot_reset` methods: they take a
  `&HolderGuard` instead of a boolean, which is the whole enforcement mechanism.
  PR #38 (cowfs-treehouse) and any real backend must adapt to that signature.
- Names, paths and refs are validated before the call, see "Validation done by the framework".
- It is `Send + Sync` and called concurrently from one thread per in-flight request.
  Handlers own their locking.
- Long operations receive an `OpContext` with `progress(event)` and `is_cancelled()`.
  `progress` returns a `cancelled` error when the request was cancelled or the connection died, so `?` stops the work.
- A panic in a handler becomes an `internal` error for that request.
- `StubHandler` keeps snapshots in memory and simulates progress and cancellation.
  It backs the tests and `cowfs serve --stub`.
  It runs every operation under one lock, which is the behaviour a real backend must match for `snapshot_rm` and `snapshot_reset`.
- The peer uid check and the directory owner check take the expected uid and a `PeerCheck` from `ServerOptions`.

## CLI

`cowfs [--socket PATH] [--timeout SECS] [--json] <command>`.

| Command | Request |
|---|---|
| `serve --store DIR --mount PATH [--stub]` | runs the server |
| `status` | `status` |
| `snapshot list` | `snapshot_list` |
| `snapshot create NAME [--from SNAPSHOT]` | `snapshot_create` |
| `snapshot rm NAME [--force]` | `snapshot_rm` (`--force` sends `expect_no_holders: false`) |
| `snapshot reset NAME --from BASE [--force]` | `snapshot_reset` |
| `snapshot rename FROM TO` | `snapshot_rename` |
| `snapshot promote NAME` | `snapshot_promote` |
| `gc [--dry-run]` | `gc` (always sends `dry_run` explicitly) |
| `fsck` | `fsck` |
| `import DIR --name NAME` | `import` |
| `base refresh --repo PATH --ref REF [--name NAME]` | `base_refresh` |
| `ps SNAPSHOT` | `ps` |
| `mount-info` | `mount_info` |
| `shutdown` | `shutdown` |
| `completions SHELL` | none, prints a shell completion script |

- The socket comes from `--socket`, else `COWFS_SOCKET`, else the default.
  An empty `COWFS_SOCKET` or `COWFS_TIMEOUT` counts as unset.
- Human output by default, with control characters escaped.
  `--json` prints the response `data` object as one line on stdout.
- With `--json`, an error is one object on stdout: `{"error": {"code": ..., "message": ..., "details"?: ...}}`, and stdout carries nothing else.
  That includes argument errors: a bad command line in `--json` mode prints the same object and exits 2.
  `--help` and `--version` stay clap text on stdout with exit 0.
  The CLI's own codes are `not_running`, `timeout`, `usage`, `io_error`, `cancelled` and `serve_failed`, next to the protocol codes.
- Progress goes to stderr and never to stdout.
  On a terminal it is a single updating bar line.
  Otherwise one line is printed when the phase changes.
  With `--json` it is one `{"progress": {phase, done, total, unit, message}}` line per event.
- Failing to write to stdout (disk full, `/dev/full`) is an error: exit 1.
  A closed pipe (`cowfs snapshot list | head -1`) exits 0, because the reader left on purpose.
- Ctrl-C (SIGINT) sends `cancel`, then waits up to 2 s for the daemon's final frame, then exits 130 either way.
  A second Ctrl-C exits 130 at once.
  SIGTERM ends the process and the daemon cancels the request when it sees the connection close.
- Paths must be valid UTF-8, see "Validation done by the framework".

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Success. |
| 1 | The daemon returned an error, or another failure, including a failed write to stdout, and a socket path that exists but is not a socket or cannot be reached (the message names the path). |
| 2 | Usage error (bad arguments, a socket path too long, a path that is not UTF-8). |
| 3 | The daemon is not running: the socket path does not exist, or it is a socket that refuses connections (stale). |
| 4 | The daemon did not answer within `--timeout`. |
| 130 | Interrupted. |

## Integration notes for treehouse (#15, #16)

- Mode (a): `ps SNAPSHOT` before `treehouse return` returns the open-fd and flock holders that treehouse misses, so the companion can kill them before the reset.
- Mode (b): slot creation is `snapshot_create {name: <slot>, from: <base>}`.
  Reset is `snapshot_reset {name: <slot>, from: <base>}`: atomic, and `busy` with nothing changed while a holder exists.
  Call `ps` first only to tell the user who holds it, never to decide: the check inside the operation is the authoritative one.
- The warm base for a repo is found with `snapshot_list` and the `base.repo` field.
  `base_refresh` is the only call that sets `repo`, `git_ref` and `commit`.
- A snapshot appears on the mount at `<mount_path>/<name>` (`mount_info` gives the mount path).
- Scripts can drive the daemon without a client library: connect, send one `hello` line, send `request` lines, read lines.
