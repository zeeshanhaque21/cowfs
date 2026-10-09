# Independent integration review of PR 96 (issue 90), critic slot 6

Target head: `02dddf8789dfc356d8685be6da9fec19fd888902`.
Scope: the three fixes for my previous round's blocking defects (error bypass, CI coverage, tracked
proof) plus the #95 elide integration, on the merged-main tree.

## Verdicts

| scope | verdict |
|---|---|
| SOURCE: FIX1 `durable_or` error model and its barrier accounting | **PASS with 3 concrete defects** |
| Real crash deliverable at this head (own harness, own mount, own daemon) | **PASS**, 2 representative reps |
| INTEGRATION: Core #95 elide + per-step barriers | **PASS**, counts exact |
| FIX2 gate: real, non-ignored, mounts, kills, reopens, cleans up | **PASS locally and in CI**, with 3 safety defects |
| FIX3 tracked evidence: present, bound to real source identities | **PASS with 1 concrete defect** |
| `docs/design.md` success criterion 2 (build overhead 1.5x native) | **OPEN, not measured** |
| Power loss / sync-vs-page-cache | **OPEN, not provable by `SIGKILL`** |
| Issue 17 / issue 88 full acceptance | **OPEN, out of scope here** |

The durability mechanism is sound and I could not break it.
What remains is test strength and teardown safety, not a wrong mechanism.

## Source identity and the base correction

Reviewed head `02dddf8789dfc356d8685be6da9fec19fd888902`, 9 commits over the merge-base.

The base correction in my task is **verified true**:
`origin/main` is `46b0f269d5bef4a2c204c25f5b3015da601d3beb`, `git merge-base --is-ancestor 46b0f26 02dddf8`
returns true, and `git merge-base 02dddf8 origin/main` is exactly `46b0f269d5bef4a2c204c25f5b3015da601d3beb`.

**The `ceb96c6` label is now wrong and must not be repeated.**
`46b0f26` is the merge-base. `ceb96c6` is merely also an ancestor, from before the merge.
`docs/verification/evidence/namespace90/README.md` still says "`ceb96c67033cbf97f79267d6af7db3fa204d77d1`,
the real merge-base, is an ancestor of this head", which reads as if `ceb96c6` were the merge-base.
It is not. The sentence is harmless in itself but it is the mislabel this review was told to correct,
so it should be fixed in the same commit that cites it.

**`crates/cowfs-core/tests/ns.rs` does not exist**, at this head or on `main`.
The blob `5a1024c115d51806131c6f6cc9ff99f5f20e6588` is `crates/cowfs-core/src/ns.rs`, source not tests.
I verified it is byte-identical in three places: `origin/main`, the head tree, and my on-disk file.
So main's bytes are preserved, and #95's Core change is genuinely in this tree.

My slot 6 worktree was at `82f370bb24a3cb667f463a15cbb9737b129cbeee`; I verified it was an ancestor of
the target before `git merge --ff-only`. No tracked edit, no commit, no push, no merge, no lease return.

The prior crash reports exist at the absolute native-12 paths, as I reported last round:
`crash88-final.md` 20729 B and `crash88-repair-final.md` 19556 B. Availability confirmed again, not inherited.

## What I ran, with true exit codes

Every rc below is the process's own, read without a pipeline. Disk before the build: 261 GiB avail.

Scoped, per the wave's "no full workspace" rule.

| command | rc | result |
|---|---|---|
| `cargo build -p cowfs-cli -p cowfs-daemon --tests` (head, under `mac-heavy.lock`) | 0 | Finished, 5.11s |
| my harness, 1 rep dir-fsync + 1 rep read-only-fd (under lock) | 0 | 2/2 KEPT |
| `cargo test -p cowfs-core --test model` | 0 | 2 passed, 0 failed, 0 ignored |
| `cargo test -p cowfs-core --test elide_dentry` | 0 | 8 passed, 0 failed, 0 ignored |
| `cargo test -p cowfs-core --test ns_durability_elide` | 0 | 3 passed, 0 failed, 0 ignored |
| `cargo test -p cowfs-core --test ns_durability` | 0 | 5 passed, 0 failed, 0 ignored |
| `cargo test -p cowfs-core --test ns_durability_cost` | 0 | 1 passed, 0 failed, 0 ignored |
| `cargo test -p cowfs-nfs --test ns_durability` | 0 | 5 passed, 0 failed, 0 ignored |
| `cargo fmt --all -- --check` | 0 | clean |
| `cargo clippy -p cowfs-nfs -p cowfs-daemon -p cowfs-core --all-targets` | 0 | 0 warnings, 0 errors |
| `cargo test -p cowfs-daemon --test namespace_durability_gate -- --test-threads=1` (under lock) | 0 | 1 passed, 0 failed, 0 ignored, 20.88s |

The Core order the task required is respected: model, elide, and the new elide tests ran **before**
NFS `ns_durability` and before the real mounted gate.

Deliberately not repeated, per the dispatch document: `cargo test --workspace`, `cargo clippy --workspace`,
the stress suite, and the `#[ignore]`d wide matrix `namespace_durability.rs`.
I make **no** claim about those suites at this head, and in particular I make no "Core 279 passed" claim.

### My own tiny deliverable, first, before any batch

Real `cowfs-daemon --backend core`, real `cowfs` CLI, real NFSv3 loopback mount, private store,
`snapshot create snap`, file written and `fsync`ed before the rename so only the name is at stake,
`SIGKILL` of a pid verified by argv containing this run's `--store` and `--socket` plus unchanged
start time, unmount, then a **fresh daemon and a fresh mount on the same store**.

| case | verdict | hash pre-rename == post-reopen | old name gone | fsck | kill exit | gap | reopen pid |
|---|---|---|---|---|---|---|---|
| `fsync-parent-dir` | KEPT | yes `4f9fbadf30c3ee63` | yes | 0 problems | -9 | 57 ms | 86192, differs |
| `fsync-read-only-fd` | KEPT | yes `4f9fbadf30c3ee63` | yes | 0 problems | -9 | 52 ms | 86392, differs |

Both read the new name back with bytes identical to the pre-rename hash, and both `fsck` clean.
No `os.sync()` is ever issued by my harness, so nothing machine-wide rescues a result.
No receipt was downgraded or relabelled.

I did not re-run the 12-rep matrix or the old-base control this round.
The mechanism verdict is inherited from `docs/reviews/namespace-durability90-final.md` at `82f370b`
and is explicitly **not** a fresh independent measurement of this head beyond the 2 reps above.

## FIX1 audit: the actual effect and error model, not helper invocation

`durable_or` is:

```rust
match (self.durable(dir), made) {
    (Ok(()), Ok(_)) => Ok(()),
    (Err(e), _) => Err(e),
    (Ok(()), Err(e)) => Err(*e),
}
```

Semantics, read rather than assumed:
barrier error wins over the change's own error, so an `NFS3ERR_IO` from the barrier is never
replaced by a status that would read as "that did not happen";
when the barrier succeeds and the change's step failed, that step's status propagates and is no
longer masked;
both fine is fine.

The arms no longer propagate with `?`, so a post-create attribute failure cannot return before the
barrier. That is the correct fix for the defect I reported, and it is at the right layer.

**The core ops make that sufficient, and I verified it rather than assuming it.**
`Inner::make`, which backs `create`, `mkdir` and `symlink`, has every fallible step
(`validate_name`, `dir`, `alloc_virt`, the `dent_lookup` existence check) **before** the first
mutation, and after `q.push(Op::Create, ...)` the path to `Ok(a)` has no fallible step.
`op_link` is the same shape: its one `?` that could look dangerous,
`s.attr.nlink.checked_add(1).ok_or(Error::TooManyLinks)?`, cannot have mutated because the assignment
never happens.
So a name mutation in these ops is all-or-nothing, and the only way a name could exist while the RPC
answers with an error is the adapter's own post-create step, which is exactly what FIX1 closed.

### Semantic assumptions I am reporting, not asserting universally

The task asked me not to claim that all errors imply no mutation without a contract.
What I can support from source, and what I cannot:

- `op_rename` and `op_rmdir` have their fallible steps (`barrier_if_needed`, `require_empty`,
  `preserve_orphan`, `dent_lookup`) before the name mutation, and after `q.push` there is no
  fallible step. So the **name** mutation is all-or-nothing in practice.
- `preserve_orphan` can queue a content operation and then fail reading xattrs, because
  `ensure_file`/`ensure_target` run before the `listxattr`/`getxattr` calls.
  That leaves a queued-but-uncommitted content operation, which the next barrier or tick commits.
  It is not a lost name, and I did not construct a case where it is.
- `require_empty` may run a barrier that itself fails, before anything of `rmdir` is mutated.
- I found **no** contract statement asserting any of this. The all-or-nothing property of the name
  mutations is an emergent property of the current code shape, not a documented invariant, so a
  future edit that adds a fallible step after `q.push` would reintroduce this class of bug silently.

### FIX1 defects

**1. `setattr` still pays a barrier on a refusal that changed nothing.**
`durable_or` evaluates `self.durable(dir)` unconditionally, as the first element of the matched
tuple. There is no short-circuit. So `setattr` reaching `durable_or` with `out == Err` still runs
the full namespace barrier, about 4.6 ms observed on this host.

This contradicts the shipped documentation. `docs/nfs-namespace-durability90.md`, in the section
added by this commit, says: "A refused mutation, by contrast, owes no barrier, because nothing
changed. `setattr` uses the same helper so a refused `setattr`, which changed nothing, does not pay
for one."

It does pay for one. For `create` and `create_exclusive` the claim holds, because a refusal
returns early with `return Err(stat(e))` before `durable_or` is ever reached.
For `setattr` it does not hold, and the comment above the call site says the same untrue thing.

Severity is low: an unnecessary barrier cannot lose data and cannot make a name less durable.
It is a cost defect and a documentation defect, and it fails the stated requirement for `setattr`
specifically. Minimal fix: short-circuit in `durable_or` when `made` is `Err` and the caller knows
nothing was mutated, or give `setattr` its own ordering rather than the shared helper.
This is the one place where the code and the doc disagree, which is exactly the kind of thing that
becomes load-bearing the next time someone reads the doc.

**2. The regression cannot distinguish the precedence it claims to pin.**
In `a_created_name_is_barriered_even_when_the_attribute_step_fails`, the injected `setattr` fault is
`Error::Io("injected setattr fault")`, and the injected barrier fault is
`Error::Io("store sync failed")`. `nfsstat` maps both through the same arm,
`Error::Corrupt(_) | Error::Io(_) => nfsstat3::NFS3ERR_IO` at `crates/cowfs-nfs/src/errors.rs:21`.

Both routes therefore assert `st == IO`, and both assertions hold no matter which error produced it.
The third route's message, "when the barrier fails the status must not read as if the create had
not happen", is not something the assertion can check.

What the test **does** pin correctly, and this part is good:
`Watched::created` records every name `create` was told to make, so the test distinguishes an error
reply about a name that exists from one about a name that does not, and it asserts
`c.lookup(&root, "attrfail").0 == OK` to prove the name is really there.
It asserts exactly one barrier in each of the three routes, so removing the barrier does fail it,
which makes the commit's mutation-check claim true for barrier **presence**.

Minimal fix to pin precedence: inject a `setattr` fault whose status differs from IO, for example
`Error::PermissionDenied` mapping to `NFS3ERR_ACCES`, and assert the returned status is IO.
Then precedence is observable.

**3. `rmdir` still has the exact shape FIX1 just fixed.**
In `Adapter::rmdir`:

```rust
Err(Error::NotEmpty) if self.opts.appledouble == AppleDoubleMode::Hide => {
    self.purge_sidecars(target.ino)?;           // unlinks N sidecar names: a real mutation
    self.vfs.rmdir(d.ino, name).map_err(stat)?; // if this fails, `?` skips durable at 727
}
```

`purge_sidecars` loops `self.remove_one(dir, &name)?`, and `remove_one` calls `self.vfs.unlink`.
So the sidecar names are really unlinked from the snapshot queue, and if the k-th unlink fails, or if
the following `vfs.rmdir` fails, the function returns through `?` and never reaches
`self.durable(d.ino)?` at line 727. The unlinked names are then queued and unbarriered with an
error reply out, which is the same defect as the one FIX1 closed in `create`.

Reachability is narrower than `create`'s was: it needs `appledouble == Hide` and a non-empty
directory whose entries are all sidecars.
It is still the same bug and, in my judgement, the one defect from this round I would fix before
merging rather than file, because it is literally the pattern the commit exists to eliminate.
`mkdir`, `symlink` and `link` are clean: their `durable` call precedes the `fa` call, and their Vfs
op is all-or-nothing.

## INTEGRATION: #95 elide plus per-step barriers

This is the question my previous round left open and refused to settle by reading.
`ns_durability_elide.rs` settles it by running both shapes, and it is the right shape of test.

| suite | rc | counts |
|---|---|---|
| `core --test model` | 0 | 2 passed, 0 failed, 0 ignored |
| `core --test elide_dentry` | 0 | 8 passed, 0 failed, 0 ignored |
| `core --test ns_durability_elide` | 0 | 3 passed, 0 failed, 0 ignored |

Counts are exactly the model2 / elide8 / new3 the task predicted.
The elide regression seed `9aa30bfa88a2438194d3b5ae7af55c7e2a59ff8a233abfe1cd0ddbec9d213900`
is present in `model.proptest-regressions` and `model.rs` still drives proptest, so the seed is
still re-run.

These three assert real state, not that a helper was invoked:
`c.stats().elided` is compared before and after a barrier, so the test observes the elide counter
rather than trusting the call;
after the barrier the test re-creates the name and requires success, proving the elided create did
not reserve it;
the no-barrier shape and the per-step-barrier shape are both run, and in both the **base and the
fork** are checked for names and for bytes after a cache drop and a reopen, with the fork's bytes
asserted to be its own;
`rmdir` has its own case, asserting the fork's directory is gone from meta after a barrier and a reopen;
every case ends in `c.check()`, which is `fsck`.

So the integration conclusion I can support: with #95's elide fix and the barrier both present, the
elided create does not reserve a name across a barrier, the elide cannot fire once a barrier has
committed the create, and names and bytes are correct in both snapshots after a reopen.
That is **PASS at the Core level**.

Still open, and I want to be exact about it: none of this runs the elide sequence through the real
NFS mount, so the integration is verified against the Core API with `sync_namespace` called directly,
not against the adapter's once-per-RPC barrier placement. An adapter-level integration test, where
the elide fires between two real RPCs with a real barrier in between, does not exist.

## FIX2: the gate is real, and CI actually runs it

The two bugs my previous round flagged are genuinely fixed, and the commit says why in comments
that match the code:

- `gate_dir()` canonicalizes before anything is mounted, with a comment naming the exact failure
  ("`is_mounted` compares the mount table verbatim, and the table holds the canonical path, so an
  unresolved `../..` would make a mounted filesystem look unmounted").
- `start()` records `self.pids.push(child.id())` and `self.child = Some(child)` **before** any
  fallible assertion, with a comment naming the leak it caused ("Asserting first leaked a live
  daemon and a live mount, and `Drop` then walked the mount").

Locally, once, under the resource lock: rc 0, 1 passed, 20.88 s, both cases ran, `LEAK` lines zero,
zero leftover daemons, zero leftover mounts.

In CI at the exact head, from the run log, not from the job conclusion:

| runner | gate binary | result |
|---|---|---|
| `check (macos-latest)` | `Running tests/namespace_durability_gate.rs` | `test a_synced_namespace_survives_a_killed_daemon_in_ci ... ok`, `1 passed; 0 failed; 0 ignored; 0 filtered out; finished in 21.55s` |
| `check (ubuntu-latest)` | `Running tests/namespace_durability_gate.rs` | `0 passed; 0 failed; 0 ignored`, 0 tests collected |

So the macOS gate **executed and passed** in 21.55 s, which is the load-bearing evidence and not a
green tick. My previous round's "zero CI coverage" defect is closed for the macOS transport.
The file is `#![cfg(target_os = "macos")]`, so on Linux it collects nothing, which is correct because
the NFS loopback is the macOS transport and Linux uses FUSE. I verified it is a whole-file cfg, not
three ignored tests; the "3 ignored" I first attributed to the gate on ubuntu was the wide
`namespace_durability.rs` matrix, which is `#[ignore]`d and does collect on Linux.

The gate's own `available()` assert refuses to report success if `/sbin/mount_nfs` is missing, which
is the right shape for a gate: on a host that cannot mount it fails naming what refused rather than
skipping. I could not exercise that branch here because the capability is present; I am reporting the
code path, not a run of it.

### FIX2 defects, all in teardown rather than in the assertion

**4. `Drop` signals bare pids with no identity check.**
`Drop` iterates `self.pids` and sends `SIGKILL` to any pid that answers `kill -0`.
`sigkill` only does `debug_assert!(self.pids.contains(&pid))`, which is compiled out in release.
So the sole guarantee is "this test once started this pid number".
There is no argv, store, socket, or start-time check before a signal, and a pid recycled between the
daemon's death and `Drop` would be signalled.
My own harness checks all four and refuses to signal on any mismatch; the gate should do the same,
and the cost is a few lines.
Low probability, real consequence, and it is precisely the discipline the dispatch document requires.

**5. The delete decision in `Drop` fails open.**
`Drop` treats `is_mounted() == false` as permission to `force_remove_dir_all(self.mount)`.
`is_mounted` returns **false** in two situations that do not mean "not mounted":

- `crates/cowfs-daemon/src/mounts.rs:59-61`: if `/sbin/mount` cannot be executed, it returns `false`.
- `cowfs_nfs::is_listed` at `crates/cowfs-nfs/src/mount.rs:291` is
  `format!(" on {} (nfs", path.display())` matched with `contains`, and there is no unescaping
  anywhere in that file, while `mount` prints a space in a path as `\040`.
  Its own unit tests at lines 479-482 only use space-free paths.

So a path containing a space, or any failure to run `mount`, yields false, skips the `umount`, skips
the `LEAK ... leaving the tree alone` preserve-return, and reaches a recursive delete on a mounted
filesystem.

Reachability today is low: the gate's paths are `bench/out/durability90-gate/mnt-<tag>` with tags
`fsync-parent-dir` and `fsync-read-only-fd`, no spaces. So this is latent, not live.
It matters because `Drop` is the last line of defence and its failure mode is destructive, and
because the same predicate guards the `LEAK` preserve-return, so a false negative removes the
evidence of the leak too.

I did **not** execute the falsifying spike. I wrote it
(`bench/out/durability90-critic/spike_drop_space.py`) and it mounts a private daemon at a path
containing a space, captures the real mount table, and evaluates the exact transcribed predicate
against it. Twelve attempts to take the wave resource lock all returned `resource lane busy; blocked`,
so per the dispatch document I did not run it unlocked. **This defect is source-verified only.**
The fix is to make the destructive branch require positive proof of absence, for example a mount
table that parsed successfully and contained no entry for the path, and to treat an unparseable or
unavailable table as "unknown, preserve".

**6. The `umount` in `Drop` is unbounded.**
`Command::new("/sbin/umount").arg("-f")...status()` blocks until `umount` exits.
The 30 s deadline that follows bounds only the polling loop **after** `umount` returns.
If `umount` wedges in uninterruptible sleep, `Drop` blocks indefinitely and takes the whole test
binary with it, which in CI is a job-level timeout rather than a clean failure.
The task noted crash 91 fixed a comparable hang and warned against assuming `Drop` is bounded; that
warning is justified here. I did not attempt to reproduce a wedge, because doing so safely would mean
deliberately wedging the kernel, which is out of bounds.

## FIX3: tracked evidence

Present on a fresh checkout, which was my first question:
`docs/verification/evidence/namespace90/README.md` 6265 B and
`docs/verification/evidence/namespace90/results-this-commit.jsonl` 1725 B, both tracked at this head.

**The source binding is sound, and I verified it blob by blob rather than trusting the table.**
All seven blobs in the README's `source-binding` table match the actual head, and all seven are
**identical** between `c644547` and `02dddf8`; the only difference between those two commits is
documentation. So evidence produced at `c644547` is source-identical at head `02dddf8`, and the
binding transfers. That is the derivation the task asked for, and it holds.

**The "before" column is labelled correctly.** The README does not claim it is `ceb96c6`; it says the
pre-fix binary was "built with every barrier call removed from `adapter.rs`", and that removing those
call sites is exactly the pre-fix behaviour because nothing else reaches `sync_namespace`.
That is the honest label for a regenerated mutant and I endorse it. I did **not** independently
regenerate that mutant; the build was never lock-available, so I am not claiming that number.

**Defect 7. The tracked receipts carry no commit or source binding of their own.**
Every one of the 12 rows has exactly these keys:

```
case, rep, pid, new_name, old_name, fsck
```

There is no rev, no source blob, no binary digest, no harness digest, and no verdict field.
The commit binding exists only in README prose, so a reader of the JSONL alone cannot tie it to a
commit, and cannot mechanically confirm the rows came from the harness the README names.
The content is otherwise sound: all 12 rows have `new_name: true`, `old_name: false`, an `ok:` fsck
line, and 12 distinct pids, and there is no demote, downgrade, reclassify, waive or
`expected_unsupported` key anywhere, so the no-downgrade requirement holds.
This is the same class of weakness critic 12 raised on PR 91, where a resume key carried no revision
and no digest. Adding a `rev` and a `harness_sha256` per row, or one header record binding the file,
would close it.

**The filename is defensible only because I checked, not on its own.**
`results-this-commit.jsonl` was measured at `c644547`, not at `02dddf8`.
The README states `c644547` explicitly, so it is not misleading to a careful reader, and since no
source changed the name happens to be true in substance.
But the name alone would not be, and that is worth stating in the file or renaming.

**Binary digests I did not reproduce.** The README lists three fixture binary sha256 values from the
builder's own target directory. I built my own pair at this head and got different digests, which is
expected. I am not claiming the builder's three, and the README's own claim that they came from a
named commit is the kind of claim that needs the same blob-level derivation I applied to the sources.

**Honest limits are all present and correct.** `SIGKILL` is not power loss and every readback is
equally consistent with the host page cache; the native APFS control is stated as recipe-positive
only, not as a durability result; the cost figures are stated as one observed distribution from one
busy host in a debug build, explicitly not an upper bound and not a benchmark; criterion 2 is stated
as unmeasured; mid-`gc`, concurrency, reclamation and `shutdown` are stated as unsampled.
The limits section is the strongest part of the change.

## CI at the exact head

One read, no dispatch, no rerun, no runner cleanup.

| check | conclusion | window |
|---|---|---|
| `check (ubuntu-latest)` | success | 23:10:34Z to 23:19:14Z |
| `check (macos-latest)` | success | 23:10:39Z to 23:21:46Z |
| `linux-fuse` | success | 23:10:34Z to 23:15:16Z |

All three completed at `02dddf8789dfc356d8685be6da9fec19fd888902`.
The builder's "ubuntu passed, mac and linux pending" snapshot has resolved to green.
Both `check` jobs ran `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -D warnings`,
`cargo test --workspace`, and the bench unittest discovery, all successful.
CI is **green**, so there is no red to block on; the earlier green at `82f370b` was a different
commit and says nothing about this one, and I am reporting this head's own runs.

I could not retrieve per-job logs through the check-runs logs endpoint, which returned zero bytes for
both runners. I got the evidence from the workflow run log instead, which is why the gate-execution
claim above is a log quote and not an inference from a green tick.

## Gates that remain open

- **Criterion 2**, build overhead within 1.5x native on a representative `cargo build` and `git status`,
  is unmeasured. The barrier adds one metadata sync plus one store sync per mutating namespace RPC,
  observed at 4.6 ms on this host against 0.005 ms for a queued rename, and a build on the mount
  performs thousands of those. Material, and still the number to demand before merge.
- **Power loss** is not addressed by any of this. Every readback I took is equally consistent with the
  host page cache, and the native control cannot distinguish it by construction.
- **GC** interaction: mid-`gc` crash, the two internal orderings between pack fsync, watermark advance
  and metadata commit, reclamation, and `shutdown` as a crash boundary are all unsampled here and
  remain open from crash 88.
- **Adapter-level elide integration**, as noted above: verified through the Core API, not through the
  real mount with real RPC timing.
- **Issue 17 and issue 88 acceptance** are out of scope for this review and are not closed by it.
  Nothing here should be read as cowfs being broadly done.

## Safety record

Everything I signalled was mine, verified immediately before the signal.
I started private daemons detached with `start_new_session=True`, per `environment-traps`.
Every kill required the pid's command line to contain this run's `--store` and `--socket` and its
start time to be unchanged, so a recycled pid is refused rather than signalled.
No process group, no `pkill -f`, no foreign pid, ever.
The real gate test I invoked is a fixture I do not own; I ran it once, under the resource lock, and
confirmed afterwards that it left zero daemons and zero mounts and printed zero `LEAK` lines.

Final state, verified:

- my worktree clean at `02dddf8789dfc356d8685be6da9fec19fd888902`, with only my untracked reports
- zero leaked daemons of mine, zero leaked mounts of mine
- shared daemon 15263 untouched: ppid 1, still started `Sat Oct  3 20:44:29 2026`
- the shared mount at `/Users/zeeshanhaque/.cowfs/mnt` still present, count 1
- 16 pool slots present; I ran no `treehouse` command of any kind, and no return, prune or destroy
- PR 91 untouched and not used as an acceptance source; other workers' trees and fixtures untouched
- no CI dispatch, no rerun, no runner cleanup, no signal to any shared process

Two things I could not do, stated plainly rather than worked around:
the wave resource lock stayed busy for roughly 35 minutes of attempts across 12 tries, so the
mutation-baseline rebuild and the `Drop` safety spike did not run;
and I did not attempt a kernel wedge to reproduce the unbounded `umount`.

## Recommendation

The durability fix itself is sound, the evidence is honestly bounded, the gate is real and CI runs
it. I would merge this as the fix for issue 90.

Before merging, fix defect 3, the `rmdir` `purge_sidecars` path, because it is the identical pattern
the commit exists to eliminate and it will be read as "already handled".
Fix defect 2's assertion while the test is being touched anyway, since it is a few characters and it
is the difference between a test that pins precedence and one that cannot.

File as follow-ups, not merge blockers: defect 1, the `setattr` doc-versus-code contradiction;
defect 4, pid identity before signals; defects 5 and 6, the fail-open and unbounded teardown, which
matter more once this gate runs on every PR; defect 7, receipts without a commit binding.
Correct the `ceb96c6` merge-base sentence in the same commit that cites it.

And keep saying what this does not show: criterion 2 is unmeasured, `SIGKILL` is not power loss, and
issue 90 closing is not cowfs closing.