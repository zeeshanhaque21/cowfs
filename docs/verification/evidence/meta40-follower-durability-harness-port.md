# #40 follower-before-leader-fsync harness port: evidence

Lane: one of the five recorded #40 mutation controls. Subject: issue #40 "follower acked before the
leader's fsync ... The critic's harness kills the first; port it."
Scope: NEW `crates/cowfs-meta/tests/follower_durability.rs` only. No production, manifest, CI, or existing-test edits.

## Original harness provenance (located, not guessed)

- Harness: `spikes/nfs-loopback/out/critic8b/work/crates/cowfs-meta/tests/crit2.rs` (git-ignored spike scratch).
  Tests `b_durable_group_commit_4_threads` / `d_durable_background_8_threads` use `Ack::Durable` with a store whose blocks are durable only after `sync`, plus per-crash-image invariants.
- Recorded mutant: `spikes/nfs-loopback/out/critic8b/mut/fol/crates/cowfs-meta/src/db.rs`.
  Exact diff vs `work`: at `wait_durable`, replaces the `gc_cv.wait_timeout` follower wait with `if *led { return Ok(()); }`.
  Critic report `spikes/nfs-loopback/out/critic8b/report.md` item 1 / summary: `follower-acked-early` KILLED lane-locally (fol.log B=407, D=602 `ACKED-DURABLE` violations) but SURVIVES the builder suite.
- Kill log: `spikes/nfs-loopback/out/critic8b/mut/fol.log` lines 183/184/190/191. Mutate log `mutate.log`: "follower-acked-early: SURVIVED 154s".

## Port

- File: `crates/cowfs-meta/tests/follower_durability.rs` (self-contained; inlines the crash-model backend, no `mod common`).
- Discriminator: invariant "an `Ack::Durable` call returns only after its change is durable, follower included", asserted at the exact return instant by comparing the caller's applied root (`Snapshot::root`) with the root in the file's committed table (`Meta::durable_snapshots`).
  Under the mutant a follower returns with `durable_seq < seq`; its edit is in no committed tree, so those two roots differ and the assertion fires.
  A final reopen from the crash image (`synced_image`, only fsynced writes) requires every acknowledged create to survive; the test also asserts commits were shared (`hook runs < calls`) so the follower path was actually exercised.
- Boundary note: the follower/leader split is decided by a private lock in `db.rs`; no public seam forces a caller into the follower branch at a chosen instant (every in-flight path also holds the session write lock the leader needs), so the branch is exercised concurrently while the durability assertion stays deterministic per call.
  The harness does not sleep to fake a pass and does not check only the leader.

## Runtime proof

- PENDING. READY5 is over the resource cap (no local cargo/build/test/mutation). No executed result is claimed here; source prediction is not executed mutation evidence.
- Branch `test/meta-follower-durability-40`, head `d819d17a7114ca6d3a2b79322c5912cd8a74def8`, draft PR #146 (Refs #40), base `origin/main` `1580e69b9d987f63c07b2430f8c0b4547ecd8622`.
- CI at push: 3 checks pending (`check ubuntu-latest`, `check macos-latest`, `linux-fuse`); not polled. CI is the first execution.

## Not claimed

- No mutation run and no pass claimed. The four other #40 controls and whole #40 remain open.
- `docs/v1-core.md`, `progress/*`, and all historical receipts left untouched; no primary commit or push.
