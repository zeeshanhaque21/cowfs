# NFS #21: READDIR cookie audit (main bc3ea7d)

Scope: source read at bc3ea7d33e3ca55ccb14e0f418a7fd223e3032d8 only.
No worktree changes, no cargo runs.

## (a) Cookie origin

Verdict: meta-assigned and stable. The adapter forwards it and builds no index.

- Assignment: `crates/cowfs-meta/src/tx.rs:122-130` takes `d.next_cookie`, increments it, and stores the cookie in both the by-name and by-cookie rows.
- The counter is per directory, starts at 1, and is never reused (`docs/v1-meta.md:279-281`).
- Resume: `crates/cowfs-meta/src/read.rs:97` scans by-cookie keys from `cookie + 1`. `read.rs:103-106` reads the cookie from the key. `read.rs:115` sets `next_cookie` to the last entry's stored cookie.
- Removal deletes only that entry's cookie row (`tx.rs:141`).
- Same-directory rename re-stores the old cookie (`tx.rs:473-476`, spec at `tx.rs:413-415`).
- Adapter: `crates/cowfs-nfs/src/adapter.rs:897-924` pages with `cookie = e.cookie` taken from the Vfs entry.
- `adapter.rs:963` copies `e.cookie` into the NFS entry.
- Hidden AppleDouble entries still advance the cookie (`adapter.rs:924`).
- `purge_sidecars` pages the same way (`adapter.rs:767-777`).
- READDIR and READDIRPLUS both use this path (`adapter.rs:902`, `912`, `1123-1130`).
- No array index or offset appears in any of these paths.

## (b) Tests

Meta level, mutation between pages:

- `crates/cowfs-meta/tests/posix.rs:327` `readdir_is_stable_and_resumable_under_removal`: unlinks hardlinks between 5-entry pages, asserts no duplicate and no dropped survivor.
- `crates/cowfs-meta/tests/posix.rs:367` `readdir_cookie_survives_removing_the_cookie_entry`: removes the last-returned entry, asserts the resumed page returns the rest.
- `crates/cowfs-meta/tests/review.rs:502` `renaming_every_listed_entry_terminates`: renames each entry as listed, asserts 50 entries, unique cookies, and a stable full listing.
- `crates/cowfs-meta/tests/critic.rs:826-858`: renames every entry, but only prints (`eprintln!`, line 854) and asserts nothing.

NFS adapter level, mutation between pages: NONE found.

- `adapter.rs:1282` covers hidden entries without mutation.
- `protocol.rs:299` and `requirements19.rs:386` page through a directory. I did not read their full bodies, so treat them as unverified.

## (c) Change needed

No change is needed for cookie stability, since the cookie is already a stable meta id.
The gap is NFS-boundary test coverage only.

- Proposed test: in `cowfs-nfs/tests`, page with `readdir_page` at 3 entries per page while `create`, `unlink`, and same-directory `rename` run between pages. Assert no skip and no duplicate for both READDIR and READDIRPLUS.
- Naming nit: `crates/cowfs-meta/src/types.rs:183` calls the cookie a "Position cookie". Reword it to "Stable entry cookie" so it does not suggest a position.

## Verdict

STABLE
Evidence: per-directory monotonic meta cookies (`tx.rs:122-130`, `read.rs:97-115`), forwarded unchanged by the adapter (`adapter.rs:924`, `963`).
Existing tests: meta-level removal and rename tests (`posix.rs:327`, `posix.rs:367`, `review.rs:502`).
Missing: no NFS-level test with create, remove, or rename between READDIR pages.
Residual #21: if the issue claims position-based behavior, the source at bc3ea7d does not support it.
