# Meta #40 M2: fail-closed pending-window policy

Issue: #40, item M2 (one transient corrupt read poisons the handle, and the pending window is discarded on drop).
Branch: `test/meta-m2-pending-window-40`, based on `main` 460e1ae.
Commit: e7d9da5 `test(meta): pin M2 fail-closed pending-window policy (#40)`.
Test file: `crates/cowfs-meta/tests/m2_pending_window40.rs`, one test.

## Policy

Decided: keep fail-closed.
A handle that has detected `Error::Corrupt` never persists state derived from it.
Pending unsynced changes are discarded on drop.
No unpoison and no forced commit were added.

## What the test proves

1. A backend with transient read bit flips (`flip_every: 3`) holds 300 durable files after `sync()`.
2. One further create stays unsynced, and is visible in the session before the flips start.
3. Flaky reads run until one returns `Error::Corrupt` (a panic counts as detection, the same way `review.rs` treats it).
4. Before drop, `health().poisoned` is true, and a further create is refused with `Error::Corrupt`.
5. After drop, the image reopens with `check()` passing.
6. The directory holds exactly the 300 durable names, so the unsynced `pending` file is absent.

The absence of `pending` is the documented, bounded loss.

## Limits

- Not run locally. Per the task, no `cargo build` or `cargo test` was run on this machine, so the test is unverified until CI runs it.
- `rustfmt --edition 2021 --check` passes on the new file.
- One flip pattern and one size (300 files) only. This pins the policy for that case, not for every corruption shape.
- No API problems: every name used is public (`Meta::open_with_backend`, `health()`, `Be` fields and `image`/`from_image`, `Snapshot` methods, `Error::Corrupt`).
- CI result: pending.
