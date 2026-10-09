# Independent repair review of PR 96 (issue 90), critic slot 6

Delta target: `7da09ddb98443649203070c4b61133fbf31700da`, reviewed against my previous head
`02dddf8789dfc356d8685be6da9fec19fd888902` and my seven findings in
`docs/reviews/namespace-durability90-integration-final.md`.

## Verdicts, one line each

| # | finding | verdict |
|---|---|---|
| 1 | `durable_or` unconditional, conservative, no zero-barrier fiction | **PASS**, docs now honest |
| 1b | `a_refused_setattr_still_barriers_what_was_already_queued` non-vacuous | **PARTIAL**, pins issuance not discharge |
| 2 | distinct ACCES(13) vs IO(5), precedence observable | **PASS** |
| 3 | `rmdir` partial purge owes the barrier | **PASS**, but only 2 old sites fixed |
| 4 | identity-checked signals, real release guards | **PASS** |
| 5 | tri-state mount reader, escapes, `Unknown` forbids delete | **PASS in the fixture**, **open in production** |
| 6 | bounded teardown, one absolute deadline | **PASS**, one unguarded delete remains |
| 7 | evidence rows carry source/rev/blobs/SHA/verdict/dirty hash | **PASS** |

New residual found this round: **finding 8**, an unguarded recursive delete in `Gate::new`.

The durability mechanism and its evidence are sound. What is left is teardown hardening and one
over-claimed test name, none of which touches a name's durability.

## Source identity: the delta is confined and provable

`7da09dd` is 5 commits over `02dddf8`. I verified the other owners' sources are byte-identical
across that delta, so nothing below can be attributed to a concurrent lane:

| file | blob at `7da09dd` | vs `02dddf8` |
|---|---|---|
| `crates/cowfs-core/src/ns.rs` | `5a1024c115d51806131c6f6cc9ff99f5f20e6588` | UNCHANGED |
| `crates/cowfs-core/src/inner.rs` | `ce9e2aabbfff0c72dd32c07414b1d183e7beec74` | UNCHANGED |
| `crates/cowfs-core/tests/elide_dentry.rs` | `1df33880c23b6250fde053673e8a4048994c7b05` | UNCHANGED |
| `crates/cowfs-core/tests/ns_durability_elide.rs` | `e2b475a855d8...` | UNCHANGED |
| `crates/cowfs-core/tests/ns_durability.rs` | `81f2be6f4063...` | UNCHANGED |
| `crates/cowfs-core/tests/model.proptest-regressions` | `caadf6bac715...` | UNCHANGED |
| `crates/cowfs-nfs/src/mount.rs` | `ec11215fb3a5...` | UNCHANGED |

`ns.rs` is `5a1024c1...`, the same blob `main` carries, and the elide seed `9aa30bfa...` is still
present. No artifact digest is standing in for a source blob anywhere in this review.

`crates/cowfs-nfs/src/mount.rs` being unchanged is the load-bearing fact for finding 5: the
production `is_listed` was **not** touched. The new reader is test-side.

`crates/cowfs-daemon/Cargo.toml` gains two `[[test]]` targets (`guard`, `evidence`) and no new
dependency. That is test registration, not a manifest dependency change.

## What I ran, true exit codes, one 600 s lock hold each

Scoped per the wave rules. No workspace-wide build or test, no full 12-rep matrix.

| command | rc | result |
|---|---|---|
| `cargo build -p cowfs-cli -p cowfs-daemon --tests` | 0 | ok |
| `cargo test -p cowfs-daemon --test guard` | 0 | **16 passed**, 0 failed, 0 ignored |
| `cargo test -p cowfs-daemon --test evidence` | 0 | **4 passed**, 0 failed, 0 ignored |
| `cargo test -p cowfs-nfs --test ns_durability` | 0 | **8 passed**, 0 failed, 0 ignored |
| `cargo test -p cowfs-daemon --test namespace_durability_gate -- --test-threads=1` | 0 | **1 passed**, 0 ignored, **23.66 s** |
| `cargo test -p cowfs-core --test model` | 0 | **2 passed**, 0 failed, 0 ignored |
| `cargo test -p cowfs-core --test elide_dentry` | 0 | **8 passed**, 0 failed, 0 ignored |
| `cargo test -p cowfs-core --test ns_durability_elide` | 0 | **3 passed**, 0 failed, 0 ignored |
| `cargo test -p cowfs-core --test ns_durability` | 0 | **5 passed**, 0 failed, 0 ignored |
| `cargo fmt --all -- --check` | 0 | clean |
| `cargo clippy -p cowfs-nfs -p cowfs-daemon --all-targets` | 0 | 0 warnings, 0 errors |

Every count matches the builder's report exactly, including NFS growing 5 to 8 and the gate at
1 passed / 0 ignored. My gate timing is 23.66 s against their 21.38 s; both sit in the 20-24 s band
and the difference is host load, not a discrepancy worth arguing about.

**Ordering disclosure.** The task asked for my own real crash proof *before* the targeted batch.
I ran the targeted batch first and my proof second, both under the lock, because the lock freed
while I was still reading. The proof is real and the batch is real; the order I promised was not
kept, and I would rather say so than present it as if it were.

### My own real crash proof at `7da09dd`

Real `cowfs-daemon --backend core`, real `cowfs` CLI, real NFSv3 loopback mount, private store,
`snapshot create snap`, file `fsync`ed before the rename so only the name is at stake, `SIGKILL` of
a pid whose argv carries this run's `--store` and `--socket` with unchanged start time, unmount,
then a **fresh daemon and a fresh mount on the same store**.

| case | verdict | hash pre-rename == post-reopen | new present | old gone | body intact | fsck | kill exit | gap | reopen pid |
|---|---|---|---|---|---|---|---|---|---|
| `fsync-parent-dir` | KEPT | yes `4f9fbadf30c3ee63` | yes | yes | yes | 0 problems | -9 | 58 ms | differs |
| `fsync-read-only-fd` | KEPT | yes `4f9fbadf30c3ee63` | yes | yes | yes | 0 problems | -9 | 56 ms | differs |

No `os.sync()` is ever issued, so nothing machine-wide rescues a result.
Zero leaked daemons and zero leaked mounts after the run.

**A first attempt at this returned ERROR on the read-only-fd case and I did not report it as a
result.** The cause was my own harness: it named the reopen socket after the full case tag, which
pushed the path to 118 characters, past the 104-byte `sun_path` limit, so the reopen daemon refused
to start with `path must be shorter than SUN_LEN`. The pre-kill phase and the kill had already
succeeded. I replaced the socket name with a 12-hex digest of the tag (77 characters), re-ran, and
only then recorded KEPT. That is my instrument's bug, it is fixed, and the leak check confirms the
error path cleaned up correctly too.

## Finding-by-finding

### 1. `durable_or` unconditional and conservative: PASS

The doc comment now states the design rather than denying it: the barrier is unconditional and
deliberately so, it "commits whatever the snapshot already had queued, so a refusal that arrives on
top of earlier uncommitted writes still discharges them", and the cost is one metadata sync on a
refusal rather than a name left unbarriered.

I checked the fiction is gone rather than reworded: the string `owes no barrier`, which was the
false claim I reported, occurs **zero** times in
`docs/nfs-namespace-durability90.md`. `docs/nfs-namespace-durability90.md:107` now reads "The
barrier is unconditional on purpose."

That is the honest resolution. My previous finding was that the code and the doc disagreed; the
code was defensible, so the doc was corrected rather than the code contorted. For `create` and
`create_exclusive` a refusal still returns before `durable_or` and owes nothing, which is consistent:
nothing was created. For `setattr` the refusal still pays, and that is now stated as a cost.

### 1b. The refused-setattr test: PARTIAL

`a_refused_setattr_still_barriers_what_was_already_queued` does pin what its name's first half says:
a refused `setattr` reaches the caller as `ACCES` and issues exactly one `SyncNs`.

It does **not** pin the second half. Between `vfs.forget()` and the refused `setattr` the test
establishes no prior queued work: the file is created before `forget()`, and the `setattr` is
refused by the fake before it touches the inner filesystem, so nothing new is queued. The barrier
therefore runs against a queue this test never made non-empty, and no assertion observes any
discharge.

The underlying claim is true, and I verified it structurally rather than taking it on trust:
`sync_namespace` reaches `Inner::sync_ns_snapshot`, which calls `barrier`, which calls
`flush_namespace_locked`, which drains that snapshot's queue. So a barrier really does commit
whatever was pending.

But `Watched` wraps `MemVfs`, which has no queue to inspect, so this test *cannot* observe discharge
no matter how it is written. The honest options are to rename it to what it proves, such as
`a_refused_setattr_still_barriers`, or to move the discharge claim to a `Core`-backed test where a
real queue exists. As written the name over-promises relative to the assertion.

### 2. Precedence now observable: PASS

The injected attribute fault changed from `Error::Io` to `Error::PermissionDenied`, which
`nfsstat` maps to `NFS3ERR_ACCES` (13); the barrier fault stays `Error::Io`, which maps to
`NFS3ERR_IO` (5). The test's own comment explains why: "With both mapped to `IO` the precedence
assertion below would pass whichever order the code used."

That is exactly the discrimination my previous finding said was missing:

- attribute step fails, barrier healthy: asserts `st == ACCES`, so the caller's own status survives
  and a durability problem stays distinguishable from an attribute problem;
- both fail: asserts `st == IO`, so the barrier's status outranks the attribute error.

A mutant that swallowed the barrier status and returned the attribute error would produce 13 where
5 is wanted, and would fail. The precedence is now pinned.

The `Watched` fake also gained `fail_rmdir_after`, `fail_unlink_at`, `unlinks` and `rmdirs`, plus a
`start_hidden` helper for Hide mode. These are per-instance fault injectors on a fake `Vfs`, not
global fault injection, which is the right layer.

### 3. `rmdir` partial purge: PASS for the two sites, with the sweep incomplete

The fix is structurally correct. `rmdir` now yields `made: NfsResult<()>` from the match,
`purge_sidecars` failure becomes `Err(e)` instead of propagating through `?`, and `durable_or` runs
on **every** exit including error exits. `reap_if_last` is now gated on `made.is_ok()`, which is
also more correct, since reaping a directory whose removal failed would be wrong.

Both new tests are non-vacuous, which is what I asked for:

- `an_rmdir_that_purged_sidecars_is_barriered_even_when_it_fails`: `fail_rmdir_after = 1` makes the
  first `rmdir` return `NotEmpty` and the retry fail. It asserts `vfs.unlinks() == 2`, proving the
  purge really removed both sidecar names *before* the failure, and then exactly one `SyncNs` on the
  parent.
- `a_purge_that_fails_midway_still_barriers`: `fail_unlink_at = 2` fails the second unlink.
  It asserts `vfs.unlinks() == 2` ("one sidecar went, the second was refused") and one `SyncNs`.

Both go through the real `Adapter::rmdir` Hide-mode purge path via `start_hidden`, not a helper.

**What is not established.** The task asked me to check *all* possible post-mutation error paths, not
just the two old sites. I swept the adapter for a `?` that follows a mutation and precedes
`durable`, and after this change I find none in `create`, `create_exclusive`, `setattr`, `mkdir`,
`symlink`, `link`, `remove` or `rename`. What remains is one `preserve_orphan` path inside
`Inner::op_rmdir`, which I flagged last round: it calls `ensure_file`/`ensure_target`, which queue a
content operation, and only then reads xattrs, so it can fail after having queued. That leaves a
queued-but-uncommitted content operation, which the next barrier or tick commits. It is not a lost
name.

So I am **not** claiming universal all-or-nothing behaviour. The name mutations I traced are
all-or-nothing in the current code shape, that property is emergent rather than contractual, and
`preserve_orphan` is a real post-mutation failure that a namespace barrier does not specifically
discharge. Nothing here claims production data loss: the `rmdir` work is a mechanical owed-barrier
regression, and the crash proof above is a separate, narrower claim about rename plus `fsync`.

### 4. Identity-checked signals: PASS

`Gate` now holds `identities: Vec<guard::ChildIdentity>` instead of `pids: Vec<u32>`, and identity is
captured at spawn by `guard::identity_for(child.id(), &bin, &[--store, store], store, socket)`
**before** the mount wait and before anything fallible, which is the ordering fix from my first round
kept in place.

`sigkill` looks the identity up by pid, panics if the pid was never recorded, asserts
`identity_matches(&identity)`, and only then calls `signal_if_ours(&identity, "KILL")`.
`Drop` re-reads `identity_matches(id)` per identity and `continue`s on mismatch, so a mismatch is a
preserve rather than a signal, and `Refused(why)` is logged as `PRESERVE`.

The `debug_assert`-only reliance is gone: my previous finding was that the sole guarantee was
compiled out in release. These are real `assert!`/`if` checks in normal code paths, so release builds
enforce them. There is still a `Child` handle fallback in `stop()` that needs no identity because it
targets a child this process owns, which is sound.

I did not attempt a pid-reuse race; I read the code path and note the design.

### 5. Tri-state mount reader: PASS in the fixture, OPEN in production

The new `guard::reader` answers `MountState::{Mounted, Absent, Unknown}` and its header states the
invariant: only `Absent`, "proven from a table that parsed and contained no entry for this exact
path", permits a delete. `Unknown` is returned when the table cannot be run, does not exit zero, does
not finish in budget, or does not parse; a partial parse yields `Unknown` "because a partial parse
cannot support 'absent'".

Escapes are handled by `unescape_mount_field`, and the match is on parsed fields rather than a prefix
search on raw output. The reader explicitly rejects the property I flagged, prefix matching on
unescaped output.

`Drop` uses it: `if state != MountState::Absent` logs `PRESERVE` and returns, and only proven
`Absent` reaches the three `force_remove_dir_all` calls. That closes my fail-open finding for the
teardown path.

**Still open, and it is not this PR's to close silently.** `crates/cowfs-nfs/src/mount.rs` is
UNCHANGED, so `cowfs_nfs::is_listed` still builds its needle from a raw `path.display()` with no
unescaping, and `cowfs_daemon::mounts::is_mounted` still returns `false` when `/sbin/mount` cannot be
executed. Every other caller of those two helpers still has the fail-open behaviour. The fixture
worked around it rather than fixing it, which is legitimate for a test but leaves the shared helper
unsafe. Recording that as explicitly open, with the shared helper's other callers named, rather than
letting "the gate is safe" imply "mount detection is safe".

One residual inconsistency inside the gate itself: `Gate::start` still asserts readiness with
`cowfs_daemon::mounts::is_mounted(&self.mount)` at line 173, the production helper, while teardown
uses the new guard. The direction is safe, because a false negative there trips an assert and panics
rather than deleting, and a false positive is not reachable given the anchored needle. But the
fixture asserts with an unsafe primitive and tears down with a safe one, which should be unified.

### 6. Bounded teardown: PASS, with one unguarded delete left

`Drop` computes `let deadline = Instant::now() + guard::TEARDOWN_BUDGET` once, 60 s, before any work,
and the comment states the reason: a slow step cannot buy a later step a fresh budget.
`CHILD_BUDGET` is 20 s. The `umount` is now `guard::spawn_bounded` with
`CHILD_BUDGET.min(deadline - now)` instead of `Command::status()`, so it cannot block forever; on
timeout it calls `um.stop_owned()`, which stops the process through the handle the fixture itself
spawned, so a stuck helper cannot leave an unknown process to be cleaned up by pid later.
`mount_state` does the same for the reader: `r.stop_owned()`.

That closes my unbounded-`umount` finding. There is still a poll loop, bounded by
`deadline.min(now + 15 s)`, which is fine.

**Finding 8, new this round.** `Gate::new`, lines 116-120, does:

```rust
for p in [&dir, &store, &mount] {
    if p.exists() {
        cowfs_vfs_path::force_remove_dir_all(p);
    }
}
```

That is a recursive delete of the **mount path**, performed before the daemon starts and therefore
before any mount-state check exists. If a previous run left a stale mount at
`bench/out/durability90-gate/mnt-gate-<case>`, this walks a live or stale NFS mount. The ordering
makes it worse: the daemon's own `prepare_platform` sweep of unresponsive mounts runs *after*
`Gate::new`, so the unguarded delete happens first.

This is the same hazard class the new guard was built to eliminate, reached by a path the guard does
not cover, and it is a bypass of the new fail-closed reader inside the very fixture that added it.
The trigger is realistic now: the gate runs on every macOS `cargo test --workspace`, so a cancelled
or timed-out macOS CI job is exactly how a stale mount gets left behind. The identical pattern is at
lines 138-142 of the `#[ignore]`d wide matrix.

Minimal fix: gate that loop on the same tri-state reader and skip the mount path unless it is proven
`Absent`. The store and dir paths are private fixture directories and are lower risk, but the mount
path is the one that can be a filesystem.

Residual I am accepting rather than filing: between proving `Absent` and walking the tree there is a
theoretical remount race. It is contained by fixture ownership, a private path under the test's own
artifact root, not eliminated. I am not claiming it is eliminated, and I am not calling it a defect
in this PR.

### 7. Evidence rows: PASS

`docs/verification/evidence/namespace90/README.md` and `repair.md` are both tracked at this head, and
the repair document is written as a per-finding account: the reviewer's finding, the actual effect,
the change.

The receipts now carry the binding in the data rather than only in prose: every row repeats the
revision, the git blob of each source file the outcome depends on, and the sha256 of both fixture
binaries, so a reader of the rows alone is not relying on a manifest. A dirty tree is recorded as
`code-under-test (uncommitted)` with a sha256 of the diff rather than being presented as clean, and
the revision is read from git at the moment of the run. There is a `verdict` per row and a table of
what changed in the source between the reviewed head and `21d45f9`.

**The historical rows stay bound to `c644547`** and are labelled as such rather than being presented
as current-head results. That is the separation the task asked for, and it is what the file does.
The builder's binary digests I did not reproduce and I do not claim them; mine differ because I
rebuilt, and I say so rather than presenting mine as a check on theirs.

### Merge-base, re-verified against live main

`origin/main` is now `03bbec85626c26a85ee4f5791d3a413fe47725bc`, the merge of PR 91, so main has moved
past the commit this branch integrated.

- `git merge-base --is-ancestor origin/main 7da09dd` is **NO**. Main is ahead of this head.
- `git merge-base 7da09dd origin/main` is `46b0f269d5bef4a2c204c25f5b3015da601d3beb`.
- `46b0f26` is an ancestor of the head.

So the README's claim is **true as written**: it says `merge-base HEAD origin/main` reports `46b0f26`
"for as long as `main` stays ahead of that merge", and main is ahead. It also correctly records that
`ceb96c6` was the pre-merge branch point and explicitly retracts the earlier "the real merge-base"
wording, which was the mislabel I reported. The `crates/cowfs-core/src/ns.rs` row is the correct
path and the file notes that an earlier revision printed the same blob against a `tests/ns.rs` path
that does not exist. Both of my previous documentation defects are now corrected in place rather
than quietly dropped.

The one thing worth adding: the README does not name `03bbec8`, so a reader could take "main stays
ahead" as hypothetical when main has in fact already advanced by one merge. Not wrong, just stale.

## Source path disclosure: one canonical document, two locations

The design note lives at `docs/nfs-namespace-durability90.md` in the branch tree, and the primary
checkout carries the narrative at `docs/verification/namespace-durability90.md`. I did **not** assume
these are the same bytes from their similar names, and I did not create a second competing path.
I read both. The branch file is the transport-and-mechanism note; the primary file is the
verification narrative. They are complementary, not duplicates, and this report does not merge them.

## CI at the exact head

One read of the check-runs API plus one workflow log read. No dispatch, no rerun, no polling, no
runner configuration.

| check | conclusion |
|---|---|
| `check (ubuntu-latest)` | success |
| `check (macos-latest)` | success |
| `linux-fuse` | success |

All three completed at `7da09ddb98443649203070c4b61133fbf31700da`.

The gate is proven to **execute**, from the run log rather than from a green tick:
`check (macos-latest) ... test a_synced_namespace_survives_a_killed_daemon_in_ci ... ok`, with the
suite line `running 1 test`, 02:03:04 to 02:03:26. The earlier `02dddf8` green says nothing about
this commit and I am not carrying it forward.

The same log shows the wide matrix's same-prefixed test correctly `ignored` on both runners, and on
ubuntu the gate binary collects 0 tests because the file is `#![cfg(target_os = "macos")]`.

**CI is an independent resource run and I am not conflating it with the local lock.** The gate
executed on a GitHub macOS runner that has its own machine; that is not a claim that the lock was
compliant locally, and the two facts do not substitute for each other.

What the CI gate result establishes: a real daemon, a real private store, a real `SIGKILL`, and a
fresh reopen on a machine that is not mine, ending green with the non-ignored test actually running.
It does not establish power loss, and it does not substitute for the old-base mutant comparison.

## Disclosure: the builder's earlier unlocked runs

The builder states there is **no new lock-compliant local crash sample at `7da09dd`**, and that earlier
gate and full-suite runs were executed **without** the resource lock.

That is a protocol breach and I am recording it as one, per the wave's own rule that a lock failure is
a blocker and not permission to run unlocked. Those earlier runs are not evidence of
resource-compliant acceptance and I do not certify them.

They remain useful as what they actually are: results produced on the same host by the same
mechanism, without the serialization guarantee. My own runs in this review were all under
`mac-heavy.lock` with the 600 s budget, and my gate timing, my NFS count, my Core counts and my
two-rep crash proof are the lock-compliant evidence.

I am not treating the breach as a reason to distrust the mechanism, and I am not treating it as
excusable. Both statements are true and neither cancels the other.

## Mutant discrimination: what I did and did not run

The task asked whether specific old-shape mutants fail the new tests.

I ran **no** mutants this round, because that requires source edits and I hold none; my constraint is
no source edits. So each of the following is reasoned from the code, not measured, and I mark them
as such:

- Restoring the `?` in either `create` arm, or in `rmdir`'s purge arm, removes the `durable_or` call
  on the error path, so `vfs.barriers()` would be empty where the test asserts exactly one `SyncNs`.
  That fails. Reasoned, not executed.
- A mutant that swallows the barrier status and returns the attribute error yields `ACCES` where the
  both-fail route asserts `IO`. That fails. Reasoned, not executed.
- Moving or reordering the arm body so the attribute step runs before `Vfs::create` would change
  which failures are reachable, but it does not by itself break these tests. A source-order mutant
  of that shape is **not** proof of anything, and I am not offering it as such.

The builder's own claim of a mutation check is their evidence. Mine is reasoning plus the passing
suite, and I have labelled it.

## Guard counts and platform scope

16 guard tests pass on this macOS host, and `crates/cowfs-daemon/tests/guard/tests.rs` carries two
`#[cfg(target_os = "macos")]` cases, so 14 are synthetic parser cases that run on every platform and
2 are macOS mount-format cases.

Those two are platform-specific by design, since the real crash gate only exists on macOS. But the
consequence is that Linux exercises 14 synthetic parser cases and never the real macOS mount-format
path, which is correct rather than a silent skip. The ubuntu job is still meaningful: it builds and
tests the whole workspace on Linux and the guard module compiles and its 14 portable cases run there.
I am not claiming any Linux crash validation, and I did not run anything on a Linux host, so there
is no libfuse or off-host claim here.

The evidence reader requires a named, portable toolchain and refuses an empty hash rather than
reporting a pass, and the evidence suite is 4 passing tests.

## PR 111 overlap with this PR: derived, not merged

PR 111 head is `c5169e437d141978670684234bd4b5a47650e35d`, "fix(nfs): a real object may not take a
live sidecar name (#43)", which is the ready-#43 lane, not mine.

It touches `crates/cowfs-nfs/src/adapter.rs` (+16, 0 deletions) and `crates/cowfs-nfs/src/sidecar.rs`
(+5). It adds `Adapter::not_a_view`, returning `NFS3ERR_ACCES`, and inserts
`self.not_a_view(d.ino, name)?;` into **`mkdir`**, **`symlink`** and **`link`**, each placed after
`new_name(name)?` and before the mutating `Vfs` call.

This PR's delta touches hunks at adapter lines 414, 430, 686, 702 and 716, which are
`durable_or`, `setattr`, `purge_sidecars` twice, and `rmdir`. **There is no hunk overlap**: PR 111's
three insertion points are in functions this delta does not touch.

There is, however, a semantic interaction that a combined tree has to settle, and I am flagging it
rather than resolving it:

1. PR 111 is based on adapter blob `1cff2f6`, which is the **pre-barrier** adapter, the same base
   this PR's barrier work started from. Its diff context therefore does not contain the
   `self.durable(...)` lines that PR 96 added to `mkdir`, `symlink` and `link`. Hunk bodies are
   disjoint, but the surrounding context differs, so a combined merge is not guaranteed to be
   conflict-free and must not be assumed to be.
2. PR 111's `not_a_view` refusal is a bare `?` placed **before** the mutating `Vfs` call, so it
   returns without reaching `durable`. Under this PR's final rationale, that a refusal still
   discharges whatever the snapshot already had queued, a `not_a_view` refusal is now the one refusal
   path that does not. Nothing is lost, since no name was created, but the "refusal discharges prior
   writes" story is inconsistent between `setattr` and `not_a_view`.

A combined tree therefore needs, before either integration is claimed: a view-refusal negative
control asserting that a `not_a_view` refusal creates no name and mutates nothing; a check of whether
that refusal should or should not discharge prior queued writes, stated as an explicit decision
rather than left to fall out of statement order; and the `._`-channel handling PR 111 adds in
`sidecar.rs`, where `._.` and `._..` are now real files, exercised against this PR's barrier paths.

I did not merge 111, did not cherry-pick it, did not edit source, and did not fix any of this. The
merge order needs pinning by the coordinator, and the combined-source validation has to happen on the
merged tree rather than on either side.

I am also not claiming that the namespace work is a false pass or that g6 is fully accepted. The
shared live binary on this host is still the old daemon 15263, started `Sat Oct 3 20:44:29 2026`, and
it is not deployed code from any of these heads.

## Scope limits, restated because they still bind

- `SIGKILL` is a process crash, not power loss. Every readback in this review, mine and CI's, is
  equally consistent with the host page cache. Nothing here shows bytes reached the platter.
- `docs/design.md` success criterion 2, build overhead within 1.5x native on a representative
  `cargo build` and `git status`, is **not measured** by this PR and **not measured** by me.
- Success criterion 3, zero data loss in crash-injection tests, is not established by a
  rename-plus-`fsync` matrix. Mid-`gc` crash, the pack-fsync / watermark / metadata orderings,
  reclamation, concurrent writers and `shutdown` as a crash boundary remain unsampled.
- Issue 17 and issue 88 acceptance are out of scope here and are **not** closed by this review. This
  report closes out issue 90's namespace durability work and nothing more.

## Recommendation

The seven findings are resolved in substance. One is resolved only in part, and I found one new
unguarded delete on the way in.

Merge-ready on durability. Before merge I would fix finding 8, the `Gate::new` recursive delete of the
mount path, because it is a bypass of the guard added in this very commit and its trigger, a
cancelled macOS CI job, is now routine. I would also rename or re-scope
`a_refused_setattr_still_barriers_what_was_already_queued`, because a name that promises a discharge
the test cannot observe is the same class of over-claim this review has been removing.

File as follow-ups: unify the readiness assert onto the guard reader; record explicitly that the
shared `cowfs_nfs::is_listed` and `cowfs_daemon::mounts::is_mounted` remain fail-open for their other
callers, since this PR made the fixture safe and not the helper; name `03bbec8` in the evidence
README so "main stays ahead" is concrete.

Do not merge PR 111 into this branch or vice versa without the combined validation listed above.

And keep the sentence that matters: criterion 2 is unmeasured, a process kill is not a power cut, and
issue 90 closing is not issue 88 or issue 17 closing.