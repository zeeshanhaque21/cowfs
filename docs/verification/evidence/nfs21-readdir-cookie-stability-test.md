# NFS #21: READDIR cookie stability across edits, test receipt

Branch: test/nfs-readdir-cookie-stability-21
Base: bc3ea7d33e3ca55ccb14e0f418a7fd223e3032d8
Commit: fce30ff48a48aad90b32e2c242c1c9a031c8f62c
Only file changed: crates/cowfs-nfs/tests/readdir_cookie21.rs (new, 94 lines).

## What it asserts

Two tests share one body: `readdir_cookies_stay_stable_across_edits_between_pages` (READDIR) and `readdirplus_cookies_stay_stable_across_edits_between_pages` (READDIRPLUS).

Setup:

- A fresh server over `memfs()` with default mount options.
- Creates f00 to f39.
- Reads page 1 with dircount 120.
- The server packs dircount / 24 entries per READDIR page (`crates/nfsserve/src/nfs_handlers.rs:776`), so page 1 holds 5 entries.

Edits between page 1 and the rest:

- Removes the first entry already returned.
- Removes the first entry not yet returned.
- Renames the second entry not yet returned to `moved`.
- Creates `new`.

Then it reads from page 1's last cookie until EOF, bounded to 100 pages.

Assertions:

- Every reply status is NFS3_OK.
- The entry removed after it was returned appears exactly once overall.
- The entry removed before it was returned appears zero times.
- The old name of the renamed entry appears zero times.
- Each surviving original name (not removed, not renamed) appears exactly once.
- `moved` and `new` each appear at most once. Either outcome is allowed.
- No name repeats anywhere in the full listing.

## Limits

- No local runtime: cargo was not run because of local disk limits. Compile and test results are pending CI.
- `rustfmt --edition 2021 --check` passes on the file.
- The test checks the observable listing only. It does not check cookie values.
- It pins only that page 1 has between 1 and 39 entries. It does not pin the page count or the exact page size.
- Backing is `MemVfs`, not the real meta or store path. Meta-level mutation tests are listed in `docs/verification/evidence/nfs21-readdir-cookie-audit.md` (`posix.rs:327`, `posix.rs:367`, `review.rs:502`).
- Paging relies on the server packing dircount / 24 entries. If that mapping changes, the test still holds for the under-40 check but may run in fewer pages.
- Does not cover AppleDouble sidecars (`._` files) during paging.
- Does not cover cross-directory rename between pages.
- Does not cover a server restart between pages.

## Status

- Commit pushed to `test/nfs-readdir-cookie-stability-21`.
- Draft PR opened from that branch to `main`. CI not awaited.
