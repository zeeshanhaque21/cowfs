# Patches on top of nfsserve 0.11.0

Base: https://github.com/huggingface/nfsserve at 0.11.0 (BSD-3-Clause, `LICENSE`).
Everything below is ours.

## From spike 2 (`docs/spikes/2-nfs-loopback.md`)

- NFSPROC3_LINK implemented (rustc incremental compilation hardlinks its object files).
- `TransactionTracker`: the retransmission table was scanned in full on every request under a global lock, 94% of server time.
  The scan now runs at most once a second.
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
- Removed: `fs_util` (Windows and path helpers), the `demo` feature, the auto IP binding, `filetime`, `intaglio`.
- Lints: the workspace lint bar (fmt, clippy `-D warnings`), `#![allow]` only for RFC style names.
