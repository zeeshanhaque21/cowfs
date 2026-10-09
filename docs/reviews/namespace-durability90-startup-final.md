# Independent final review of PR 96 (issue 90), critic slot 6

Target: `b604aa7e8f662b8084f18bc07133f77b06173b53`, against my previous head
`7da09ddb98443649203070c4b61133fbf31700da`.

## Verdicts

| scope | verdict |
|---|---|
| Startup safety: no delete at startup, refuse instead | **PASS**, my finding 8 closed |
| Cleanup single seam: positively `Absent` **and** owned | **PASS** |
| Readiness on the local tri-state reader, not the fail-open helper | **PASS** |
| Negative controls: refusal-to-delete and delete-and-still-refuse | **PASS**, marker-preserved, non-vacuous |
| Production adapter / Core / VFS unchanged, so the `7da` proof carries | **PASS**, blob-proved |
| `7da` finding 1b, test-name honesty | **PASS** |
| Own small real crash deliverable at this head | **PASS**, 2/2 KEPT |
| Scoped counts, fmt, clippy | **PASS**, all exact |
| INTEGRATION: `merge-tree` of `main` + this head | **PASS**, zero conflicts, tree `12eb07df` |
| Combined tree actual tests incl. PR111 raw RPC guards | **PASS**, 21/21 raw RPC on the combined tree |
| Queued-refusal semantics across the merge | **consistent with the documented contract**, one asymmetry for the coordinator |
| `cowfs_nfs::is_listed` / `mounts::is_mounted` fail-open for other callers | **OPEN**, not this PR's to fix silently |
| Success criterion 2, power loss, criterion 3, issues 88 and 17 | **OPEN** |

Nothing in this delta changes durability. The mechanism is byte-identical to the head I already
crash-tested, and I re-proved it anyway with a fresh fixture.

## Source identity: this delta touches no production code

`b604aa7` is two commits over `7da09dd`: `9109b9d` "startup never deletes, and it cannot see a stale
mount as absent", and `b604aa7` the documentation. The diffstat is tests and docs only.

I verified every production blob is unchanged, so my previous proof transfers by identity rather than
by assertion:

| file | blob at `b604aa7` | vs `7da09dd` |
|---|---|---|
| `crates/cowfs-nfs/src/adapter.rs` | `c9ff3862ea1647d2ca3825fc997159cd8b2e0172` | UNCHANGED |
| `crates/cowfs-nfs/src/mount.rs` | `ec11215fb3a5...` | UNCHANGED |
| `crates/cowfs-nfs/src/sidecar.rs` | `8f058a124f92...` | UNCHANGED |
| `crates/cowfs-core/src/inner.rs` | `ce9e2aabbfff0c72dd32c07414b1d183e7beec74` | UNCHANGED |
| `crates/cowfs-core/src/ns.rs` | `5a1024c115d51806131c6f6cc9ff99f5f20e6588` | UNCHANGED |
| `crates/cowfs-vfs/src/vfs.rs` | `b09e3b718b25...` | UNCHANGED |
| `crates/cowfs-core/tests/model.proptest-regressions` | `caadf6bac715...` | UNCHANGED |

`ns.rs` is still the `5a1024c1...` blob `main` carries, and the elide seed `9aa30bfa...` is still
present. No artifact digest is standing in for a source blob anywhere below.

`cowfs-nfs/src/mount.rs` still being unchanged is the load-bearing fact for the open finding: the
production mount helpers were **not** fixed, only bypassed inside the fixture.

`crates/cowfs-daemon/Cargo.toml` is unchanged in this delta, so no manifest change and no new
dependency; the two `[[test]]` targets from `7da09dd` carry forward.

## Finding 8, the unguarded startup delete: closed

This was the defect I raised last round: `Gate::new` recursively deleted the mount path before any
mount-state check, and the daemon's own stale-mount sweep runs after that, so the delete happened
first.

The replacement is `guard::attempt` plus `guard::preflight`.

`attempt` mints a fresh root per run, named from the pid, a nanosecond clock reading and a
per-process counter, so two fixtures cannot collide by construction. `preflight` then refuses rather
than cleans, and its own doc comment states the reasoning: "Cleaning it up would mean deleting a
directory nobody has yet established is not a stale mount."

Concretely `preflight` returns `Err` when `dir`, `store` or `mount` already exists, when the socket
already exists, or when the socket path does not fit `sun_path`. It never deletes anything.

It is wired into all three startup paths: `Gate::new` at line 121, `Run::new` in the ignored wide
matrix at line 143, and the native control at line 560. Each panics on refusal with the reason.

That last point is the correct trade: a collision is a bug in the name generator or a reused fixed
path, and stopping with a message beats deleting something the fixture has not proven is stale.

`socket_fits` also measures the socket path **in bytes against `SUN_PATH_MAX`**, which is the exact
class of bug I hit in my own harness two rounds ago, when a readable tag overflowed `sun_path` and
the reopen daemon refused to start.

### The negative control is real, and it is safe to run

`a_cancelled_previous_attempt_leaves_its_directory_untouched_at_startup` builds a stale directory
with a `STALE-MARKER` file, then asserts three things: a fresh unique root **passes** preflight, a
colliding path **refuses** with a message naming what it found, and the marker's bytes are still
exactly `a previous attempt left this`.

So a mutant that turned the refusal back into a delete would destroy the marker and fail. That is the
mutation-sensitive control, and it needs no dangerous experiment: the "stale mount" is a plain local
directory, never a real mounted filesystem, and I did not run any recursive delete against a live or
stale mount.

The same round adds controls for the other properties: `a_recycled_pid_is_refused_on_the_start_time`,
`a_command_that_outlives_its_budget_is_reported_not_awaited`,
`a_command_that_finishes_inside_its_budget_is_reported_finished`,
`only_a_proven_absent_owned_path_is_deleted`,
`an_absent_path_this_fixture_did_not_create_is_never_deleted`,
`a_mounted_or_untrusted_path_never_reaches_the_delete`,
`a_failed_remove_is_reported_as_preserved_not_as_removed`,
`the_socket_path_is_measured_in_bytes_not_characters`,
`the_socket_stays_under_the_short_temp_dir_however_long_the_root_is`,
`a_fresh_attempt_mints_a_path_that_does_not_exist_yet`,
`the_real_table_is_readable_and_classifies_a_private_path`.

`a_failed_remove_is_reported_as_preserved_not_as_removed` is the marker-preserved property I asked
for: a failed delete is reported as preserved, never as removed.

## Cleanup single seam, and readiness no longer bypasses it

`guard::cleanup` is now one seam, and its doc is explicit that ownership is required on top of
absence: "`Absent` alone is not enough: a fixture that never made...". It takes the store and socket
the fixture owns and only deletes a path it both owns and has proven absent.

The gate's readiness check now reads `self.mount_state()`, the local tri-state reader, with a comment
naming what it is avoiding: "The local tri-state reader, not `cowfs_daemon::mounts::is_mounted`.
That helper...". That closes the residual inconsistency I reported at `7da09dd`, where the fixture
asserted readiness with the production fail-open helper and tore down with the safe one.

One number needs stating precisely, because it is easy to misread as a product budget:
`TEARDOWN_BUDGET` is 60 s and `CHILD_BUDGET` is 20 s. **Those are fixture teardown budgets, not a
runtime API budget.** Nothing in this change bounds or alters a production operation, and the
per-RPC barrier cost measured earlier, around 4.6 ms on this host, is unchanged because the adapter
is unchanged.

## `7da09dd` finding 1b, the over-claiming test name: closed

The test is now `a_refused_setattr_still_issues_a_namespace_barrier`. It says **issues**, which is
what it proves.

Its doc comment also states the limit I flagged, rather than papering over it: "The discharge claim
is true, but it is verified structurally rather than here", followed by the chain
`Vfs::sync_namespace` to `Inner::sync_ns_snapshot` to `barrier` to `flush_namespace_locked` which
drains that snapshot's queue, and the reason it is not observed here: doing so "would need a real
`Core` with a queue that can be read back after a reopen, which this seam has no business standing
up."

That is the honest split I asked for: the mechanism is asserted structurally, the test claims only
what it can see, and the reason for the gap is given rather than hidden.

I independently re-verified that chain in the production source rather than accepting the comment,
and it holds: `sync_namespace` dispatches to `op_sync_namespace`, which for a non-root inode calls
`sync_ns_snapshot`, which calls `barrier`, which calls `flush_namespace_locked`, which drains that
snapshot's queue and commits the batch.

## What I ran, true exit codes

One 600 s lock hold for the build, my own proof first, then the batch; a second for the mounted gate;
a third for the combined tree. Disk was 417 GiB free before any build, far above the 20 GiB floor.
Scoped throughout: no workspace-wide build or test, no cold dependency build, no full 12-rep matrix.

| command | rc | result |
|---|---|---|
| `cargo build -p cowfs-cli -p cowfs-daemon --tests` | 0 | ok |
| my harness, 2 variants, 1 rep | 0 | **2/2 KEPT** |
| `cargo test -p cowfs-daemon --test guard` | 0 | **24 passed**, 0 failed, 0 ignored |
| `cargo test -p cowfs-daemon --test evidence` | 0 | **4 passed**, 0 failed, 0 ignored |
| `cargo test -p cowfs-nfs --test ns_durability` | 0 | **8 passed**, 0 failed, 0 ignored |
| `cargo test -p cowfs-core --test ns_durability` | 0 | **5 passed**, 0 failed, 0 ignored |
| `cargo test -p cowfs-core --test ns_durability_elide` | 0 | **3 passed**, 0 failed, 0 ignored |
| `cargo test -p cowfs-core --test elide_dentry` | 0 | **8 passed**, 0 failed, 0 ignored |
| `cargo test -p cowfs-core --test model` | 0 | **2 passed**, 0 failed, 0 ignored |
| `cargo fmt --all -- --check` | 0 | clean |
| `cargo clippy -p cowfs-nfs -p cowfs-daemon --all-targets` | 0 | 0 warnings, 0 errors |
| `cargo test -p cowfs-daemon --test namespace_durability_gate -- --test-threads=1` | 0 | **1 passed**, 0 ignored, **19.83 s** |

Toolchain: the wave's `space-bunny-free` on macOS 26, `cargo` and `rustc` from the workspace stable
toolchain via `dtolnay/rust-toolchain@stable` locally. I am labelling the toolchain I actually used
rather than guessing at the builder's.

Every count matches the target exactly. The gate ran with zero `PRESERVE` and zero `LEAK` lines, and
left zero daemons and zero mounts behind.

### My own real deliverable, first, on a fresh fixture

Real `cowfs-daemon --backend core`, real `cowfs` CLI, real NFSv3 loopback mount, private store,
`snapshot create snap`, the file `fsync`ed before the rename so only the name is uncommitted,
`SIGKILL` of a pid whose argv carries this run's `--store` and `--socket` with unchanged start time,
unmount, then a **fresh daemon and a fresh mount on the same store**.

| case | verdict | hash pre-rename == post-reopen | new present | old gone | body intact | fsck | kill exit | gap | reopen pid |
|---|---|---|---|---|---|---|---|---|---|
| `fsync-parent-dir` | KEPT | yes | yes | yes | yes | 0 problems | -9 | 57 ms | differs |
| `fsync-read-only-fd` | KEPT | yes | yes | yes | yes | 0 problems | -9 | 58 ms | differs |

No `os.sync()` is ever issued, so nothing machine-wide rescues a result. No receipt was downgraded or
relabelled.

**Digest provenance, stated precisely.** My payload hashes to `4f9fbadf30c3ee63`. The new author
commit `9109b9d2` reports `fcea6b5b` for its own KEPT run. **These are different payloads**, so the
two digests are not comparable, neither validates the other, and I am not presenting mine as a check
on theirs or theirs as a check on mine. Both are single-harness, single-host observations of the same
mechanism.

## INTEGRATION: exact `main` + this head

`origin/main` is `3a6935b244d0209ef7c75ddf9539f54dc137105b`, the merge of PR 111, and I confirmed
`c5169e437d141978670684234bd4b5a47650e35d` **is** an ancestor of it. So `main` carries the
independently reviewed LINK and AppleDouble guard. PR 96 has **not** been merged into `main`.

Merge base of the two: `46b0f269d5bef4a2c204c25f5b3015da601d3beb`.

`git merge-tree --write-tree 3a6935b2 b604aa7` produced tree
**`12eb07df703a0bbde9c99a6e9d1962663e280059`** with **zero conflicts** and no conflict markers. I
produced it and materialised it with `git archive` into my own ignored scratch at
`bench/out/namespace90-final-critic/combined`. No branch was created, nothing was committed, no source
branch changed: my worktree HEAD is still `b604aa7` with a clean tracked tree.

The combined `adapter.rs` is `bbae4d5a697dd89b35cc6eb655d3ae4c81adbc14`, which differs from **both**
parents, `abe6447...` on main and `c9ff386...` on this branch, so the blob itself is the proof that
the merge combined the two changes rather than picking a side. It contains one `durable_or`
definition with three `self.durable_or(d.ino, &made)` call sites, one `not_a_view` definition with
three call sites, and the combined tree keeps `ns.rs` at `5a1024c1...`, `inner.rs` at `ce9e2aab...`,
`sidecar.rs` at PR 111's `f8b2bfb2...`, and the `9aa30bfa...` seed.

This is the **exact tree** I tested. It is not the same overall head as the earlier PR 111 critic's
combined tree `f537dee145`, and I am not inheriting that review's results.

### Combined-tree actual tests

| suite | rc | result |
|---|---|---|
| `cowfs-nfs --test translate` (PR 111's suite) | 0 | **13 passed**, 0 failed, 0 ignored |
| `cowfs-nfs --test ns_durability` (PR 96's suite) | 0 | **8 passed**, 0 failed, 0 ignored |
| `cowfs-daemon --test guard` | 0 | **24 passed**, 0 failed, 0 ignored |
| `cowfs-daemon --test evidence` | 0 | **4 passed**, 0 failed, 0 ignored |
| `cowfs-nfs --test mount -- --ignored` (PR 111 raw RPC) | 0 | **21 passed**, 0 failed, 767.91 s |

The raw RPC guards are all `#[ignore]`d with the reason "mounts a filesystem; run with `--ignored`",
so my first combined run reported **0 passed, 21 ignored**, which is a skip and not a pass. I
re-ran them with `--ignored` and they genuinely executed: 21 passed, 0 failed, in 767.91 s, each
mounting a real NFS loopback. That is the actual combined validation, running PR 111's raw wire
guards on top of PR 96's barrier code, and it is the strongest single piece of integration evidence
in this review.

The earlier non-vacuous properties survive on the combined tree because their tests are in the
merged tree and pass there: the `rmdir` purge assertions with `unlinks() == 2`, the ACCES-versus-IO
precedence assertions, the guard channel cases, and every negative control above.

One thing I am explicitly **not** claiming: the check-then-act residual in the already-owned
`ReviewedRaceVfs` is present in both sources and is **open issue 43**. It is not a regression
introduced by this integration, I did not fix it, and I did not treat it as one.

## The queued-refusal question across the merge

The task's framing is the right one and I want to answer it precisely rather than declare a
difference wrong or wave it through.

PR 96's `durable_or` is unconditional, so a refused `setattr`, which changed nothing itself, still
commits whatever the snapshot already had queued. PR 111's `not_a_view` is a bare `?` placed in
`mkdir`, `symlink` and `link` **before** the mutating `Vfs` call, so that refusal returns `ACCES`
without reaching any barrier. I confirmed this on the combined tree at `adapter.rs:631`.

"No current mutation" does **not** prove "no prior queue", and the two refusals genuinely differ.

What I checked before deciding whether that is a defect: the documented contract. The branch document
now says the barrier is owed because a code path "may already have mutated", and it states
explicitly that "`create` and `create_exclusive` do return before `durable_or` on a refusal, because
they refuse" before anything is created. It also retracts the earlier claim that a refusal owes
none.

So the contract is *this RPC's own mutation must be durable before its acknowledgement*. A refusal
that occurs strictly before any mutation owes nothing under that contract, and `not_a_view`
satisfies it. The discharge of prior unrelated queued work is a **consequence** of `durable_or`
being unconditional, described in the document as a side benefit that costs one metadata sync, not a
guarantee that every refusal path promises.

Therefore: **no owed data is waived, and I am not calling `not_a_view` wrong.** The asymmetry is real
and worth a decision, because a `setattr` refusal discharges the queue and a `not_a_view` refusal
does not, so two refusals that both changed nothing behave differently. Missing a discharge is not
data loss: that prior work is still queued and the next barrier or tick commits it. But a caller
relying on "any namespace RPC flushes what is pending" would be wrong to rely on it, and the
document does not currently promise that.

The related trade the task names is also a contract decision, not a bug: the barrier's `NFS3ERR_IO`
outranks the attribute error's `ACCES`, on the stated grounds that a status reading as "that did not
happen" would be a lie about a name that exists. That is deliberate, it is pinned by a test that can
actually fail now that the two statuses are distinct, and it belongs to the coordinator as a product
decision rather than to me.

I did not change the guard, the barriers, the contract, and I did not rebase, cherry-pick or merge
anything.

## CI at the exact head

One read of the check-runs API and one workflow log read. No polling, no dispatch, no rerun, no
workflow or runner configuration.

| check | conclusion |
|---|---|
| `check (ubuntu-latest)` | success |
| `check (macos-latest)` | success |
| `linux-fuse` | success |

All three completed at `b604aa7e8f662b8084f18bc07133f77b06173b53`. From the log rather than from a
green tick: `check (macos-latest) ... test a_synced_namespace_survives_a_killed_daemon_in_ci ... ok`,
02:41:30 to 02:41:53. On ubuntu the gate binary collects nothing, because the file is
`#![cfg(target_os = "macos")]`.

CI is an independent resource run on GitHub's own machines. It is not a local lock-compliance claim,
and it is not a substitute for one. **I am not claiming CI on `main`, and not claiming CI on the
combined tree**: the combined tree I tested locally is a `merge-tree` result that exists in no
branch, so no CI has ever run it. The only CI statement I make is the one above, about this head.

## Disclosures carried forward

- **Three unlocked builder runs.** Earlier builder gate and suite runs were executed without the
  resource lock. That is a protocol breach under the wave rule that a lock failure is a blocker and
  not permission to run unlocked. Those runs are not certified as resource-compliant acceptance.
  Everything in this report that I measured was under `mac-heavy.lock` with a single 600 s budget.
- **Two CI reads** are two reads of the same green head, not two independent executions.
- **My `7da09dd` proof carries** into this head by blob identity, and I additionally re-ran a fresh
  2-variant fixture here rather than resting on that.
- **Native APFS control**, where present in prior rounds, remains recipe-positive only. A process
  kill cannot establish durability.
- One reviewer instrument bug is on the record from an earlier round and stays disclosed: a readable
  socket tag overflowed `sun_path` and produced a false ERROR, which I fixed before recording any
  verdict. Notably, this delta's `socket_fits` byte check now guards that exact failure at startup.

## Open, and still open

- **`cowfs_nfs::is_listed` and `cowfs_daemon::mounts::is_mounted` remain fail-open for their other
  callers.** `mount.rs` is unchanged: the needle is still built from a raw `path.display()` with no
  unescaping, and `is_mounted` still returns `false` when `/sbin/mount` cannot be executed. The
  fixture worked around both rather than fixing them. That is legitimate for a test and leaves the
  shared helpers unsafe for everyone else. This is the one item I would most want a follow-up issue
  for, because it is now the only remaining place where the same reasoning was applied in two
  places and only one was fixed.
- **Success criterion 2**, build overhead within 1.5x native, unmeasured by this PR and by me.
- **Power loss.** `SIGKILL` is a process crash. Every readback here, mine and CI's, is equally
  consistent with the host page cache. No claim that bytes reached the platter.
- **Success criterion 3**, zero data loss in crash-injection tests, is not established by a
  rename-plus-`fsync` matrix. Mid-`gc` crash, pack-fsync and watermark and metadata ordering,
  reclamation, concurrent writers and `shutdown` as a crash boundary remain unsampled.
- **Issue 43** stays open on the check-then-act residual. Issue 111 merged without closing it.
- **Issues 88 and 17** acceptance are out of scope and are not closed here.
- The shared live daemon on this host is still the old build from `Sat Oct 3`, and is not deployed
  code from any head in this review.

## Recommendation

The delta does what it claims, and it does it in the only layer where it can be done safely: test
fixtures. Production is untouched, which is why the durability mechanism and my earlier proof both
stand unchanged.

I would merge this. Finding 8, the one I raised at `7da09dd`, is genuinely closed, and closed with a
mutation-sensitive marker-preserved control that needs no dangerous experiment. The over-claiming
test name is fixed and the remaining structural claim is now labelled as structural.

File as follow-ups: a real fix for the shared `is_listed` and `is_mounted` helpers so the fixture is
not the only safe caller; and a coordinator decision on whether a refusal path should be required to
discharge prior queued writes, since `setattr` does and `not_a_view` does not.

Merge order still needs pinning by the coordinator. The `merge-tree` result is clean, so the merge is
not expected to conflict, but no CI has run the combined tree and it should not be merged on the
strength of a local test alone.

And the sentence that has to survive all of this: criterion 2 is unmeasured, a process kill is not a
power cut, issue 90 closing is not issue 88 or issue 17 or issue 43 closing, and this branch is not
merged.