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
  A peer whose uid differs from the server's is sent a `permission_denied` error and disconnected before any frame is read.
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
   A client that connects and sends nothing is dropped after the handshake timeout (10 seconds).

The version is a major version.
It is `1` for everything in this document.

## Requests and responses

Request ids are chosen by the client, unique among its in-flight requests on that connection.
A duplicate in-flight id gets `duplicate_id`.
A connection may have up to 32 requests in flight, more get `busy`.
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
| `snapshot_rm` | `{name}` | `ok`: `{}` | no |
| `snapshot_rename` | `{from, to}` | `snapshot`: `SnapshotInfo` | no |
| `snapshot_promote` | `{name}` | `snapshot`: `SnapshotInfo` | no |
| `gc` | `{dry_run?}` | `gc`: `GcReport` | yes |
| `fsck` | `{}` | `fsck`: `FsckReport` | yes |
| `import` | `{path, name}` | `import`: `ImportReport` | yes |
| `base_refresh` | `{repo, git_ref, name?}` | `base_refresh`: `BaseRefreshReport` | yes |
| `ps` | `{snapshot}` | `processes`: `{processes: [ProcessInfo]}` | no |
| `mount_info` | `{}` | `mount_info`: `{mount_path, adapter, mounted}` | no |
| `shutdown` | `{}` | `ok`: `{}` | no |

### Semantics worth knowing

- `snapshot_create` without `from` creates a snapshot of the empty tree.
  With `from` it is an O(1) writable clone of that snapshot.
  It fails with `already_exists` if `name` is taken and `not_found` if `from` is missing.
- `snapshot_rm` fails with `busy` if the snapshot is in use by a process the daemon knows about, and `not_found` if it does not exist.
- `snapshot_promote` turns a clone into a base: `base` becomes non-null with all-null fields.
  It is idempotent.
- `base_refresh` builds or refreshes the warm base snapshot for a repository at a git ref.
  If `name` is omitted the daemon derives a stable name from `repo`.
  The previous base with the same name is replaced only after the new one is complete.
  The result names the snapshot and the previous commit, if any.
- `gc` with `dry_run: true` frees nothing and reports what would be freed.
  Garbage collection is mark and sweep from snapshot roots, as in `docs/design.md`.
- `import` follows the migration rules in `docs/design.md`: ingest, verify by hash, and only then report success.
  The daemon never swaps a directory for a mount on its own: the caller does that after `import` succeeds and `verified` is true.
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

`ImportReport`: `{name, files, bytes, verified}`.

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
- A client that stops reading is treated the same way.
  The server sets a write timeout (30 seconds).
  When a write times out or fails, the connection is closed and its requests are cancelled.
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
| `internal` | A bug, including a panic in a handler. The connection stays open. |

Clients must treat an unknown code as a generic failure and show `message`.

## Evolution rules

Within major version 1:

1. Adding an optional field to any object, with a default, is compatible.
   Receivers ignore fields they do not know, and senders omit optional fields at their default.
2. Adding a method, a response `kind`, a frame `type`, an error code or a progress `phase` is compatible.
   A client learns which methods exist from `hello.methods`.
3. Adding a value to an enum-typed field (`unit`, hold `kind`) is compatible: clients map unknown values to `other`.
4. Removing or renaming a field, method or code, changing a type, or changing the meaning of an existing field is breaking.
   It requires a new major version, and the server may speak both during a transition.
5. A change that alters the wire format for any existing message fails the golden-file test in `crates/cowfs-ctl/tests/wire.rs`.
   Updating the golden file (`COWFS_UPDATE_GOLDEN=1`) is how a reviewer sees a protocol change.

## Server framework

- `ControlHandler` (`cowfs-ctl`) is the trait a daemon implements: `status`, `snapshot_*`, `gc`, `fsck`, `import`, `base_refresh`, `ps`, `mount_info`, `shutdown`.
  `ping` and `version` are answered by the framework.
- It is `Send + Sync` and called concurrently from one thread per in-flight request.
  Handlers own their locking.
- Long operations receive an `OpContext` with `progress(event)` and `is_cancelled()`.
  `progress` returns a `cancelled` error when the request was cancelled or the connection died, so `?` stops the work.
- A panic in a handler becomes an `internal` error for that request.
- `StubHandler` keeps snapshots in memory and simulates progress and cancellation.
  It backs the tests and `cowfs serve --stub`.

## CLI

`cowfs [--socket PATH] [--json] <command>`.

| Command | Request |
|---|---|
| `serve --store DIR --mount PATH [--stub]` | runs the server |
| `status` | `status` |
| `snapshot list` | `snapshot_list` |
| `snapshot create NAME [--from SNAPSHOT]` | `snapshot_create` |
| `snapshot rm NAME` | `snapshot_rm` |
| `snapshot rename FROM TO` | `snapshot_rename` |
| `snapshot promote NAME` | `snapshot_promote` |
| `gc [--dry-run]` | `gc` |
| `fsck` | `fsck` |
| `import DIR --name NAME` | `import` |
| `base refresh --repo PATH --ref REF [--name NAME]` | `base_refresh` |
| `ps SNAPSHOT` | `ps` |
| `mount-info` | `mount_info` |
| `shutdown` | `shutdown` |
| `completions SHELL` | none, prints a shell completion script |

- Human output by default.
  `--json` prints the response `data` object as one line on stdout.
  With `--json`, an error is printed on stderr as `{"error": {"code": ..., "message": ...}}`.
- Progress goes to stderr.
  On a terminal it is a single updating bar line.
  Otherwise one line is printed when the phase changes.
- Ctrl-C sends `cancel`, waits briefly for the final frame and exits 130.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Success. |
| 1 | The daemon returned an error, or another failure. |
| 2 | Usage error (bad arguments). |
| 3 | The daemon is not running (no socket, or connection refused). |
| 130 | Interrupted. |

## Integration notes for treehouse (#15, #16)

- Mode (a): `ps SNAPSHOT` before `treehouse return` returns the open-fd and flock holders that treehouse misses, so the companion can kill them before the reset.
- Mode (b): slot creation is `snapshot_create {name: <slot>, from: <base>}`.
  Reset is `snapshot_rm` then `snapshot_create`.
  The two calls are not atomic together in v1.
- The warm base for a repo is found with `snapshot_list` and the `base.repo` field.
  `base_refresh` is the only call that sets `repo`, `git_ref` and `commit`.
- A snapshot appears on the mount at `<mount_path>/<name>` (`mount_info` gives the mount path).
- Scripts can drive the daemon without a client library: connect, send one `hello` line, send `request` lines, read lines.
