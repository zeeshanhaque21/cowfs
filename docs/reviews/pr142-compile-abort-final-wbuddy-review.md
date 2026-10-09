# PR142 compile/abort correction: final independent review

READONLY. Exact head reviewed: `010f00a2bbe7c84a76935ef473478d1d0453dc17` (tree `cowfs-7c1bf8/3`).
Prior head `84e5193`; commit `010f00a` touches one file, `crates/cowfs-core/tests/critic2b.rs`, `+26/-3` (`git diff 84e5193 010f00a`). Merge base with primary `9874afae` is `93cfef94`, so this is the real head, not primary lineage.

## Source verdict: SOUND, addresses both prior blockers

- `encode_mark(2000).to_vec()` at both torn-branch sites (`:417`,`:419`): `encode_mark` returns `[u8;16]`, target is `Vec<u8>`. Both prior E0308s resolved; the two tests now build.
- `child_abort` asserts `out.status.signal() == Some(6)` BEFORE stdout parse, so a non-abort child (clean exit, panic that still returns text) fails instead of false-passing. Linux/macOS both raise SIGABRT=6; sound where `ExitStatusExt` is in scope.
- `child_abort` used only by the reservation caller (`:358`); other `child()` callers (`:142`,`:1072`) unchanged.
- `first & (1<<63) != 0` asserts the physical bit; output comes from a real `create`+`sync`, so an empty/parse failure hits `.expect(...)`, not a silent pass.
- Four damages (`zeros/delete/zero-byte/torn`) each assert the damage landed, then reopen and assert `b.ino != first`, `floor > first_meta`, old number returns no other bytes; `floor` is meta `ino.reserved` and `first_meta = first & VIRT_COUNTER_MASK` (`SHIFT=40`), same 40-bit space, not apples-to-oranges. Abort child writes neither legacy mark, so the damages are genuine no-ops against the reservation path, as intended.
- No assertion weakening; alias/conformance/Meta/session contracts unchanged (diff is one test file).

## Runtime verdict: PENDING with an observed failure (not green)

Run `37546185283` (head `010f00a`, `event=pull_request`) is `in_progress`; jobs: `check (ubuntu-latest)` `completed/failure`, `check (macos-latest)` `in_progress`, `linux-fuse` `in_progress`. Job logs are unavailable until the run completes, so the failing step (fmt/clippy/test) is unconfirmed, NOT assumable as the old E0308. A failed first job stops later "required" jobs, so no conditional green from FUSE. Prior run `37545084282` (`84e5193`) is `completed/failure`; `37541353589` (`39a8747`) predates both. New-target counts (identity 4, metadata 5, core_atomic 10, Meta T12/T14+reserve 11) are source counts only; none has an exact-head result line. Full #42 crash/session-load/git-status-cost stays open even if unit-green; do not close #42.

## Checkout / report pin

Reviewed tree is the leased worktree at head `010f00a`, not primary `9874afae`. PR body's newest section still documents `84e5193` and calls `37545084282` pending; it does not yet name `010f00a`/`37546185283`. Prior review doc `docs/reviews/pr142-physical-recovery-fixtures-final-wbuddy-review.md` (SHA256 `d198bcc8...`) and the fixture receipt are present on disk but untracked in primary HEAD (blob not resolvable), so pinned-blob reverify is SOURCE-only. Primary `v1/`/`progress/`/protected docs untouched. No local cargo/build/test/probe/checkout/signal/GitHub-mutation performed. Assertion: source fix is correct; runtime acceptance still requires a completed green `37546185283` (all required jobs) plus independent execution of the recovery fixtures.
