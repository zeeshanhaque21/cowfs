# Patches on top of nfsserve 0.11.0

Base: https://github.com/huggingface/nfsserve at 0.11.0 (BSD-3-Clause, `LICENSE`).
Everything below is ours.

## From spike 2 (`docs/spikes/2-nfs-loopback.md`)

- NFSPROC3_LINK implemented (rustc incremental compilation hardlinks its object files).
- `TransactionTracker`: the retransmission table was scanned in full on every request under a global lock, 94% of server time. It is gone, replaced by `reply_cache.rs` (below).
- READDIR and READDIRPLUS cookies are positions supplied by the file system, not file ids.
  Hardlinks in one directory share a file id, so id cookies restarted listings in the wrong place.
- Per-procedure operation count and latency table, `take_stats()`.

## For v1

- `NFSFileSystem` trait reshaped for `cowfs-nfs`:
  - `lookup` returns the object attributes too.
  - `create` takes a `guarded` flag, `create_exclusive` takes the verifier and returns attributes, `mkdir` takes attributes.
  - `rmdir` and `commit` (COMMIT, and stable WRITEs) are separate methods, `write` takes owned data and returns the count.
  - `readdir` takes a cookie and a `with_attrs` flag, plain READDIR no longer fetches attributes.
  - `fsstat` and `pathconf` come from the file system.
  - The file handle generation comes from the implementation (`generation`), no `static mut`.
- Every procedure decodes its arguments up front and answers GARBAGE_ARGS for malformed calls.
  READDIR honours its cookie.
  `SETATTR` with a ctime guard no longer writes two replies.
  ACCESS reports the owner permission bits instead of everything.
  WRITE honours the requested stability and reports UNSTABLE for unstable writes.
  MKNOD answers NFS3ERR_NOTSUPP.
- Replies leave in one write (header and body together) so TCP_NODELAY does not split them.
- No panics on network data: bounded XDR lengths and RPC message size, `unwrap` and `assert` removed, `accept` errors do not end the server loop.
- Security and bounds (`tcp.rs`, `rpcwire.rs`, `reply_cache.rs`):
  - `MountGate`: the first connection to send MNT gets the root handle, other connections are refused until `rearm`.
  - `PeerCheck` hook for MNT callers.
  - `Limits`: connection cap, idle timeout, per-frame slowloris deadline, in-flight requests per connection, frame size cap.
  - Records are read incrementally, so a declared but unsent frame costs nothing.
  - READDIR, READDIRPLUS and READ replies are capped whatever count the client asks for.
  - Reply cache (`reply_cache.rs`) for SETATTR, CREATE, MKDIR, SYMLINK, REMOVE, RMDIR, RENAME and
    LINK: a retransmitted call gets the original reply. Bounded in entries, bytes and age, and it
    shrinks.
    The key is (connection, client address, xid, hash of the whole call). The connection is part
    of it because one client keeps several connections open and each counts xids from its own
    start, so an identical call with the same xid on another live connection is a new call and is
    executed. Keying on (address, xid, hash) alone made the server answer such a call with the
    first connection's reply without ever running it, and the client was then told a file existed
    that did not.
    A call may still be replayed on another connection, but only when the connection that ran it
    has not sent anything since: a client that got the reply moved on, and a client that is
    repeating a call on a fresh connection is one whose reply was lost, which over TCP is the
    only shape a post-reconnect resend can take. The price is that a mutation is executed twice
    if a client sends the same (xid, call) on a second connection while the first connection
    never sent anything after it. No client does that, and preferring a duplicate effect over a
    silently wrong success is the safer of the two. The per-connection high-water marks are
    bounded, and the cross-connection index never outlives the entries it points at.
- Removed: `fs_util` (Windows and path helpers), `config.rs` (a builder for a whole server configuration that this server does not use), `write_counter`, `transaction_tracker`, the `demo` feature, the auto IP binding, `filetime`, `intaglio`.
- Lints: the workspace lint bar (fmt, clippy `-D warnings`).
  Allows that remain: `lib.rs` has `#![allow(non_camel_case_types, clippy::upper_case_acronyms)]` because the RFC type and procedure names are kept as written; `mount.rs` and `portmap.rs` have `#![allow(dead_code)]` because they transcribe RFC constants this server does not all use.
