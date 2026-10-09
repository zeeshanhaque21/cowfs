# PR 142 Core commit-retry consumer: independent read-only audit

Auditor: wbuddy independent READ-ONLY lane.
Date: 2026-10-06.
Scope: PR 142 (`test(core): reserved-inode consumer regression for #42 request 4`), #42 request 4 Core consumer, commit-error retry repair.
Mode: read-only.
No source edit, checkout, build, test, clippy, fmt, target, archive, probe, cleanup, offload, lease, signal, daemon, store, mount, or CI action was performed.
All evidence is pinned to immutable SHAs read from the remote.

## 1. Exact head, tree, base

- PR 142: open, draft, `merged: false`, `mergeable: false`, `mergeable_state: dirty`.
- Head SHA: `89d3a9db6df3e99d893168b0f867089e0255fabc`.
- Head tree: `f823bde1af9b9b6afa23630fc6e9b0083a04eea6`.
- Empty re-trigger commit `89d3a9d` has tree `f823bde1`, byte-identical to `976aaa4` tree `f823bde1`.
- PR base SHA: `89353e17e5085000711dc428e834f9cc41840a1f` (PR 141 merge), base tree `6ff6b446`.
- Remote branch tip `refs/heads/fix/core-reserved-inode-consumer-42` equals the head `89d3a9d`.
- Remote main is `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e`, so the PR base is behind main and the PR is `dirty`.
- Change set vs base: 20 files, +3250 / -70.

Commit chain, verified linear on the feature commits (`git log --parents`), no rewritten SHAs, one legitimate merge of a main-line branch:

```
89d3a9d 976aaa4 ci: re-trigger the pull_request workflow for this head
976aaa4 890cd27 test(meta): fix the reservation-retry tests to compile
890cd27 d7646eb fix(core): gate the mark writer's trait import with its only user
d7646eb 01c8a2c fix(meta): keep a reserved number owned until its create is durable
01c8a2c 8a6892a wip(core): reserved-ID create consumer with store and session bound tickets (#42)
8a6892a 10c4a0f 573b02f5 Merge commit '573b02f5...' into fix/core-reserved-inode-consumer-42
```

`01c8a2c` is an ancestor of the head. `8a6892a` is a merge whose second parent is `573b02f5` (the PR 140 main-line chain). Base `89353e17` is an ancestor of the head, so no history rewrite or force is required to explain the current tip. The earlier force-with-lease to `01c8a2c` (documented in the implementation receipt) predates the audited repair commits and did not replace any other worker's push.

## 2. Production scope

Diff vs base restricted to production source (tests and docs excluded):

```
crates/cowfs-core/src/inner.rs  | 46 +-
crates/cowfs-core/src/ino.rs    | 13 +-
crates/cowfs-core/src/lib.rs    |  7 +-
crates/cowfs-core/src/ns.rs     |  8 +-
crates/cowfs-core/src/queue.rs  |  3 +
crates/cowfs-meta/src/check.rs  | 13 +
crates/cowfs-meta/src/db.rs     | 121 +
crates/cowfs-meta/src/lib.rs    |  5 +-
crates/cowfs-meta/src/tx.rs     | 103 +
crates/cowfs-meta/src/types.rs  | 71 +
crates/cowfs-meta/tests/inode_reservation.rs         | 394 + (new, public)
crates/cowfs-core/tests/reserved_inode_identity.rs   | new (identity regression)
docs/... (6 receipts) and docs/v1-core.md
```

Every production hunk is in scope for #42 request 4 (Core consumer of reserved inode IDs) or is a direct consequence (dead-code removal of the superseded virtual allocator; a `check` invariant for the reservation-intent bound). No unrelated feature or refactor is present.

### Dead virtual-number path removal: legitimate

`alloc_virt`, `reserve_virt`, `next_virt`, `virt_reserved`, `virt_lock`, `VIRT_BLOCK` are removed and have zero remaining references (grep over the head tree returns none). The legacy *read* side (`read_virt_mark`, `classify`, `snap_of`, `Aliases`) stays live so stores written before meta owned the reservation still decode. The legacy *writer* (`virt`, `write_virt_mark`, `newest_copy`, and `use std::io::Write as _`) is `#[cfg(test)]`-gated with its only users, so the lib target has no unused item and the shape tests can still lay a mark down. The removal was forced by `-D warnings`, not by a scope change.

## 3. Source verdict on the retry mechanism

The repair is in `d7646eb` (`crates/cowfs-meta/src/db.rs`) and is correct in source.

### The historical defect

At `01c8a2c`, `db.rs` line 985 ran `s.reserved.remove(ino)` from the batch path, before line 1037 `self.wait_durable(seq)?`. A `wait_durable` failure therefore left the number out of the session's outstanding set while the create was not durable, so a retry reached `Tx::new_child_at` and was refused `Invalid("reserved number was not issued by this store's open session")`. Reachable only under `Ack::Durable`; Core opens Meta with `Ack::Applied`, so no existing test exercised it.

### The head fix

At the head, `mutate` collects spent numbers in a `HashSet<Ino>` that outlives the writer-lock block (line 968). Inside the block the spent numbers are removed from `s.reserved` only after the closure returns `Ok` (line 1017). If the closure returns `Err` or panics, `e.tree = saved` rolls the pending edit back and the numbers are never removed, so the ticket stays usable (pinned by the existing `a_closure_error_leaves_the_ticket_usable_for_retry`). On the durable path, `wait_durable` failure (line 1069) re-acquires the write lock and re-inserts each spent number into `s.reserved` (line 1076) before returning the error. The tree is deliberately not rolled back on `wait_durable` failure, because the writer lock is dropped inside `wait_durable` and a concurrent mutation can interleave; a blind rollback could clobber it.

### Why returning an already-spent number is safe

The prompt's hazard is that returning a spent number could authorize a different inode or name, duplicate an existing object, or duplicate one hidden elsewhere. Source analysis resolves each:

1. Authorization is by a move-only capability. `ReservedIno` is `#[derive(Debug, PartialEq, Eq)]` only - not `Copy`, not `Clone` - with private fields carrying the store id and the number. A caller cannot duplicate the ticket to spend it twice.
2. Three independent checks in `Tx::new_child_at`: `ticket.store != self.store` refuses a foreign store; `!self.reserved.contains(&ino)` refuses a number not in this open session's set; `self.spent.contains(&ino)` refuses a second spend in the same transaction. Authorization is never by raw numeric range, numeric absence, or a below-floor test.
3. The inode-exists check is the cross-name guard. `read::inode(self, ino)` reads through `Tx::get` -> `self.tree.get(self.src, key)`, i.e. the pending in-session tree, which still holds the create after a `wait_durable` failure. A retry at the same number is therefore refused `Error::Exists` regardless of directory or name. The number cannot be applied to a different name, and cannot duplicate an object, because the record at that number is already visible.
4. The re-inserted number cannot be re-minted. `s.reserved` is read only by the `create_at` authority check; the only mint path is `reserve_inodes` -> `reserve_tickets`, which advances `s.ino.next` and never draws from `s.reserved`. So no concurrent session or batch can pick the re-inserted number for a different create.
5. After drop or reopen the pending tree is discarded, but the reservation commit moved the durable floor (`ino_reserved`) before the number was handed out, so the number is never reissued. That is the documented #42 contract and the property `numbers_reserved_and_never_used_are_not_reissued_after_a_reopen` asserts.

The only reachable reuse of a returned number is the same caller's retry with the same capability, and it is stopped by the pending inode record. No cross-name, no duplicate, no hidden-elsewhere duplicate.

### The post-persist branch

`COMMIT_FAULT = 2` fails after `wtx.commit()`, so the create is durable while the caller sees an error. The retry is then refused by the same inode-exists check, because the durable record is present. This is exactly what T14 asserts. The pre-persist branch (`COMMIT_FAULT = 1`) persists nothing, the number is restored, and the retry is refused only by the still-pending tree edit - never by a lost reservation. That is T13. Both branches are driven from a `#[cfg(test)]` thread-local seam; no production fault API is added.

## 4. Runtime proof: actual runs, and what is UNEXECUTED

All facts below are from the GitHub Actions API as of this audit.

No `cargo test` result exists for any PR-142 head. Every run that reached the branch failed at the `cargo clippy --workspace --all-targets -- -D warnings` step, and `cargo test --workspace` was skipped in all of them.

| Head | Run | ubuntu step 6 (clippy) | cargo test |
|---|---|---|---|
| `01c8a2c` | 37518582016 | failure (6 dead-code lints in `cowfs-core`) | skipped |
| `d7646eb` | 37520667773 | failure `unused import: std::io::Write as _` at `ino.rs:4:5` (ubuntu + macos) | skipped |
| `890cd27` | 37521194983 | failure `error[E0369]` at `db.rs:2434:9` and `unused variable: want` at `db.rs:2459:13` (ubuntu + macos) | skipped |
| `976aaa4` | none | no run exists (`total_count: 0`) | not run |
| `89d3a9d` | none | no run exists (`total_count: 0`) | not run |

The current head `89d3a9d` has **zero** workflow runs and **zero** check-runs. This is confirmed twice: `actions/runs?head_sha=89d3a9db6` returns `total_count: 0`, and `commits/89d3a9db6/check-runs` returns `total_count: 0`. The repo-wide latest-runs list does not contain `976aaa4` or `89d3a9d`.

### Source fixes match the exact CI errors, but the fixes themselves are UNEXECUTED

- `890cd27` gates `use std::io::Write as _` with `#[cfg(test)]`; the head `ino.rs` shows the attribute and the comment. This matches the `d7646eb` clippy error exactly.
- `976aaa4` replaces the T12 `assert_eq!(..., Err(Error::NotFound))` with `matches!` (fixes `E0369` at `db.rs:2434`) and binds `want` into `assert_eq!(got.ino, want)` in T14 (fixes `unused variable: want` at `db.rs:2459`). Both edits are present at the head.

The fixes are source-verified against the literal compiler output, but no CI run has compiled or executed them. Their correctness is a source claim, not a runtime result.

### What this audit does NOT claim

- It does not claim the current head is green. There is no run to be green.
- It does not convert a source edit into a test result. `cargo test` never ran.
- It does not treat Linux FUSE conformance green as Meta or Core identity evidence. The FUSE job passes in these runs and only enforces its own known-failure list; it does not compile or run the new `inode_reservation` or `reserved_inode_identity` tests, which run under `check (ubuntu-latest)` / `check (macos-latest)` at the skipped `cargo test` step.
- It does not claim the T12/T13/T14 mutants were killed. No mutation harness was run against `COMMIT_FAULT` or the retry branch.

### Note on the earlier `10c4a0f` figure

The implementation receipt states the earlier `1 passed; 2 failed` figure is from the OLD fixture. The test file `crates/cowfs-core/tests/reserved_inode_identity.rs` is byte-identical between `10c4a0f` and the head (empty diff), so that fixture is the same regression file, and its OLD-path failures are not NEW-path evidence. `10c4a0f` also failed at clippy, not at `cargo test`; no NEW-path identity run exists anywhere.

### CI infrastructure gap (reported as runtime-blocked, not polled)

The pushing lanes report that GitHub created no `pull_request` run for `976aaa4` or `89d3a9d`, and the API confirms `total_count: 0` for both. This audit reports the absence as a runtime block and did not poll, wait, rerun, dispatch, or change any runner, workflow, or CI trigger.

## 5. Author artifacts: accurate

- The retry-repair receipt `docs/verification/evidence/meta42-core-commit-retry-repair.md` (blob on disk, SHA-256 `668b648c92fa454bcafc348721d805bcd955dc70daaf72d175525f35ac65633f`) is honest: the status line reads "source complete on draft PR #142, CI running on the pushing head"; it explicitly records the CI infra gap and states "No local build was run in place of CI, and no test was weakened". It does not claim green. This doc lives in the main checkout, not the PR head tree.
- The implementation receipt `meta42-core-reserved-inode-consumer-implementation.md` is honest: it documents the retry gap before the repair, marks compile and test proof UNEXECUTED, and lists the four remaining work items that the follow-up commits then addressed.
- The PR 142 body is honest: it states the actual gate is CI on the pushed head, documents the infra gap, and uses no closing form, no co-author trailer, and no green claim.
- The authors' summary ("only CI remains") is not adopted as a finding. This audit enumerated the prompt-to-artifact criteria independently; see section 6.

## 6. #42 request-4 criteria audit

| Criterion | Status |
|---|---|
| Capability is a minted, store- and session-bound, move-only ticket | SOURCE met (`ReservedIno` `!Copy`/`!Clone`; three checks in `new_child_at`) |
| One-time consumption; closure error leaves the ticket usable | SOURCE met; covered by a test that has never executed on this head |
| Commit-error retry does not strand the reserved number | SOURCE met (T13); UNEXECUTED at runtime |
| Persist-then-error does not duplicate | SOURCE met (T14); UNEXECUTED at runtime |
| Cross-name / hidden duplicate refused by the pending inode record | SOURCE met (`read::inode` over the pending tree); UNEXECUTED |
| Post-drop/reopen number never reissued | SOURCE met (durable floor before hand-out); covered by an author test not run on this head |
| Reverse-retry where a spend could authorize a different inode/name | SOURCE refuted by construction; no runtime proof |
| Core consumer identity (create -> write -> flush -> reopen) | SOURCE met; UNEXECUTED (no `cargo test` run) |
| Production scope contains no unrelated change | met |
| CI green on the delivered head | MISSING - no run exists for `976aaa4` or `89d3a9d` |

Not in this PR, therefore still missing for whole-#42 acceptance, and owned by other lanes: the Core `cowfs_meta::Health` wiring and batch timestamps, the crash harness over the real store, the ported mutant harness, and the remaining #42 consumer/snapshot requests. PR 142 covers request 4 only.

## 7. Verdict

MERGE_READY pending CI, with a runtime-evidence block.

- Source: the production consumer and the commit-error retry repair are correct by inspection, and the author artifacts are accurate.
- Runtime: blocked. The delivered head `89d3a9d` has no CI run; `cargo test` has never executed on any PR-142 head; the compile fixes in `976aaa4` are source-verified against literal compiler errors but UNEXECUTED.
- The #42 request-4 obligation set is SOURCE-covered; the runtime half of the acceptance is not met on this head.

A green `cargo test` on a head whose tree is `f823bde1` (i.e. `976aaa4` or a descendant with the same tree) is the minimum missing acceptance for the request-4 contract. FUSE conformance green in any run is not that acceptance.

## 8. Report integrity

- Path: `docs/reviews/pr142-core-commit-retry-wbuddy-review.md`.
- External SHA-256 of this file is reported by the auditor's shell after the final write; no self-referential placeholder is embedded. The auditor re-computes it after any edit so the reported hash matches the delivered bytes.
- All receipt and report files referenced are treated as immutable.
- No PR body, comment, issue, CI, commit, or remote ref was modified.

## 9. Tools and prerequisites

- Used: `gh api` (read-only), `gh run view --log` (read-only), `git fetch`/`git show`/`git log`/`git merge-base`/`git ls-remote` (read-only), `rtk`-fronted shell.
- Unavailable in this session: the `context-mode` MCP tools (`ctx_execute`, `ctx_batch_execute`, `ctx_search`) were not present in the tool catalog. Large-output derivation was done in-line; no artifact claiming a sandbox run is made.
- Blocked prerequisite: local `cargo build`/`test`/`clippy` is forbidden by this audit's read-only constraint and by the worktree capacity block; no local execution was attempted.
