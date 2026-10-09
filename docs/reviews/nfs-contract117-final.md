# Independent review of PR 119 (issue 117), critic slot 6

Head: `b4b55abfe9ab2d8d6f5fc42403bb1eb8b1c02d41`. Stated base: `951045f`.

## Verdict

**Test-only change: PASS. The replacement assertion is non-vacuous: PASS. Merge recommended.**

The branch does exactly one thing, it does it in a test rather than in production, and the central
claim survives an independent mutation: the old assertion could not reject anything, and the new one
can.

| check | verdict |
|---|---|
| Only `crates/cowfs-nfs/tests/contract.rs` changed, no production source | **PASS** |
| The old assertion was a tautology, and `post` was not what its name said | **PASS**, confirmed from source |
| Same public TCP caller: old passes the sticky-`JUKEBOX` mutant | **PASS**, reproduced |
| Same fixture and blob: new fails that mutant `left: 10008 right: 2` | **PASS**, reproduced |
| New unmutated passes | **PASS** |
| No atomicity invented; `LOOKUP` non-mutating; `Retry` mapping unchanged | **PASS** |
| #90 barrier and error precedence untouched | **PASS**, not in the diff |
| Scoped counts, fmt, clippy | **PASS**, all exact |
| rustc 1.95 clippy report | **NOT REPRODUCED**, as the evidence itself states |
| CI at exact head, test really executes | **PASS**, 3/3 green, 0 skipped |
| GC same-class tautology (`control.rs:75`) | **reported, not fixed**, issue 122 separate |

## Scope: test-only, verified

Single commit `b4b55ab` "test(nfs): the retry test asserted nothing, so it now asserts the contract
(#117)". Its first parent is `951045fca4823611e196eda75db0c977a46d2c77`, matching the stated base.

```
crates/cowfs-nfs/tests/contract.rs | 44 +++++++++++++++++++++++++++++++++++---
1 file changed, 41 insertions(+), 3 deletions(-)
```

`git diff --name-only 951045f..HEAD | grep '^crates/.*/src/'` is **empty**. No production source file
is touched, no `Cargo.toml` change, no new dependency. `crates/cowfs-gc` is touched zero times.

My worktree reached the head by `git merge --ff-only` after verifying `b604aa7` was an ancestor, and
`git merge-base b4b55ab b604aa7` is `b604aa7`, so this branch sits directly on the head I reviewed
last. Tracked tree clean; the mutation I made lived only in ignored archive copies.

## The defect, confirmed from source rather than accepted

`common::Nfs::lookup` is documented at line 249 as
`(status, handle, object attributes, directory attributes)` and its error arm is:

```rust
} else {
    (st, None, None, attr(dec(&mut r)))
}
```

So the fourth element is the post-op **directory** attributes, and on an error reply it is populated.
The removed line bound it to `post` and asserted `post.is_none() || true`, which evaluates to `true`
for every input: the expression cannot reject any value, and it was inspecting the opposite of what
its name claimed. The old test was green for a reason unrelated to the behaviour.

The new test destructures the same four values with the element actually named for what it is, and
asserts three things through the same public TCP seam and the same fake `Vfs`:

1. a `NFS3ERR_JUKEBOX` reply carries no object handle and no object attributes;
2. once the injected fault is cleared, an absent name answers `NFS3ERR_NOENT`, not a sticky
   `NFS3ERR_JUKEBOX`;
3. an error reply still carries the post-op directory attributes, which is the field the removed
   tautology was aimed at.

## The mutation discriminant, independently reproduced

This is the load-bearing evidence, so I ran it rather than believing it. Same fixture, same fake
`Vfs`, same public caller, same single-line production mutation, applied only to my own archive
copies: `Error::NotFound => nfsstat3::NFS3ERR_NOENT` becomes `Error::NotFound => nfsstat3::NFS3ERR_JUKEBOX`,
which makes a retry status sticky.

| tree | mutation | rc | result |
|---|---|---|---|
| `951045f` old | `NotFound -> JUKEBOX` | **0** | **1 passed** |
| `b4b55ab` new | `NotFound -> JUKEBOX` | **101** | **FAILED**, `left: 10008`, `right: 2` |
| `b4b55ab` new | none | **0** | **1 passed** |

The old test passes a mutant that makes every missing name look like a retry; the new test fails it on
exactly the numbers the evidence claims, and the new test passes unmutated. That is the difference
between removing a tautology and asserting a property, and it is now measured rather than argued.

A direct probe of the mutated build printed `st=10008 fh=false obj=false dir_some=true`, so the
`JUKEBOX` really did reach the client and the post-op directory attributes really were still present.
Both new assertions are load-bearing under the mutant, not incidental.

### My own instrument bug, disclosed

My first attempt at this table was **wrong and I nearly reported it as a finding against the branch**.
I ran all four trees against one shared `CARGO_TARGET_DIR`, and `new-mut` came back `rc=0`, passed,
which would have meant the evidence did not reproduce. It was my bug: two archives of the same
path package share a package name and version, and the reused target directory served `new-mut` a
binary built from `old-mut` source. The old, vacuous test then passed.

I caught it rather than believing it, in this order: the mutation's live-ness was proved independently
by running `errors::tests::every_error_has_its_status` in the mutated tree, which **failed** with
`left: NFS3ERR_JUKEBOX, right: NFS3ERR_NOENT`. A live mutation plus a passing consumer is a
contradiction, so the consumer was not the mutated code. Re-running with a target directory isolated
per archive produced the table above.

I am recording this because the intermediate conclusion I stated in-flight, that the evidence claim
did not reproduce, was wrong, and the corrected result is the opposite.

### What the mutant also breaks, stated honestly

The same mutation also breaks the pre-existing `errors::tests::every_error_has_its_status`. So the
new contract test is **not** the only detector of a sticky-`JUKEBOX` mutant in this crate. What the
new test adds is detection at the **protocol seam**, over a real TCP connection, asserting the
properties a client actually depends on: no object handle on a retry status, no cached retry status
after the fault clears, and directory attributes still present on an error reply. The unit test pins
the mapping table; this pins the observable contract. Those are different jobs and both are worth
having.

## No atomicity invented, and the #90 surface is untouched

The test's own doc comment states the limit correctly: "Nothing here claims atomicity. `LOOKUP`
mutates nothing, and this branch adds no guarantee about partially applied operations; the barrier and
error-precedence guarantees from #90 are asserted elsewhere and are untouched." That is right, and it
is checkable rather than rhetorical:

- `Adapter::lookup`'s `_` arm is `self.vfs.lookup(d.ino, name).map_err(stat)?`, a read. There is no
  partially applied state to specify, so the test correctly asserts statuses and reply shape only.
- `Error::Retry => nfsstat3::NFS3ERR_JUKEBOX` at `errors.rs:23` is unchanged; the branch does not
  touch the mapping.
- `crates/cowfs-nfs/src/adapter.rs` is not in the diff, so the #90 `durable_or` unconditional barrier,
  its `NFS3ERR_IO`-outranks-`ACCES` precedence, and the `rmdir` purge barrier are all untouched. The
  `crates/cowfs-core/src/ns.rs` blob at this head is still `5a1024c115d51806131c6f6cc9ff99f5f20e6588`
  and the elide seed `9aa30bfa...` is still present, so none of that priority surface moved either.

## Scoped runs, true exit codes

One 600 s foreground lock hold. Disk was 408 GiB free before any build, far above the 20 GiB floor.
No workspace-wide build, no cold dependency fetch, no full durability matrix. This work needs no real
mount: the seam is an in-process TCP server with a fake `Vfs`.

| command | rc | result |
|---|---|---|
| `cargo test -p cowfs-nfs --test contract` | 0 | **3 passed**, 0 failed, 0 ignored |
| `cargo test -p cowfs-nfs` | 0 | **145 passed**, 0 failed, 23 ignored |
| `cargo fmt --all --check` | 0 | clean |
| `cargo clippy -p cowfs-nfs --all-targets` (1.99) | 0 | 0 warnings, 0 errors |
| `cargo +1.95 clippy -p cowfs-nfs --all-targets` | **1** | **NOT REPRODUCED**, component absent |
| `cargo +1.95 check -p cowfs-nfs --all-targets` | 0 | clean under 1.95 |

Caller count is unchanged and I checked it rather than assuming: the retry test makes **2**
`c.lookup` calls before and **2** after, so the new assertions cost no extra client round-trips and
add no new dependency. `contract.rs` holds 3 `#[test]` functions in total.

### The 1.95 lint, and what is not reproduced

`cargo +1.95 clippy -p cowfs-nfs --all-targets` exits 1 with:

```
error: 'cargo-clippy' is not installed for the toolchain '1.95-aarch64-apple-darwin'.
```

**I did not install the component**, because installing one is outside this task and outside my
mandate. So the reported 1.95 clippy failure on this expression stays **NOT REPRODUCED**, and I am not
guessing the lint name.

Two things follow that are worth stating precisely:

- `cargo +1.95 check` exits 0, which establishes the report is **clippy-only** and not a rustc error.
  That is the one thing now established about the 1.95 report.
- The 1.99 clippy result **neither reproduces nor refutes** the 1.95 report. A clean 1.99 run says
  nothing about a lint that only fires on 1.95, and the expression the report names is gone from this
  branch, so whatever lint it raised can no longer fire here at all.

Toolchain labels are what I actually ran: default stable resolved as 1.99 for clippy and fmt, with
1.95 installed but lacking its clippy component. I am not inferring the builder's toolchain.

## CI at the exact head

One read of the check-runs API and one workflow log read. No polling, no dispatch, no rerun, no
workflow or runner configuration.

| check | conclusion |
|---|---|
| `check (ubuntu-latest)` | success |
| `check (macos-latest)` | success |
| `linux-fuse` | success |

All three completed at `b4b55abfe9ab2d8d6f5fc42403bb1eb8b1c02d41`, matching the claim of three green.

The test is proven to **execute**, from the log rather than from a green tick. On both
`check (macos-latest)` and `check (ubuntu-latest)`:

```
test an_error_the_vfs_asks_to_retry_becomes_the_retry_status ... ok
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

So it ran on two operating systems over the real TCP seam with **zero skipped and zero ignored** in
that binary. One caution about reading these logs: an adjacent line reads
`test result: ok. 0 passed; 0 failed; 1 ignored` on ubuntu, and that belongs to the **previous** test
binary in the log, not to `contract`. I checked the attribution rather than reporting the skip as
contract's.

## GC, same defect class, tracked separately

`crates/cowfs-gc/tests/control.rs:75` reads:

```rust
assert!(!f.gc_dir().join("mark.bin").exists() || true);
```

Identical shape: `|| true` makes it unfalsifiable. I confirmed it is still present at this head, and
that PR 119 touches **zero** `cowfs-gc` files, so it is correctly reported rather than silently fixed
or silently borrowed.

I am treating it as a separate issue (122) and I am **not** claiming any g-criterion completeness
because of it. Editing GC is not my path here, and I did not touch it.

## Closing references: only #117

The body and history of this branch close issue 117 and nothing else, and that is what I verified.

- **#117** is the subject, and on this evidence it is addressed.
- **#90** remains closed as previously intended, with nothing in this branch re-opening or
  re-scoping it. I am not renewing its scope.
- **#88** and **#17** remain **open**. This branch changes no crash-gate or namespace-publication
  state, and nothing here should be read as advancing either.
- **#43** stays open on the check-then-act residual, and **#122** is the new GC tautology.
- `docs/design.md` success criterion 2 remains unmeasured, and `SIGKILL` remains a process crash
  rather than a power cut, unchanged by anything in this branch.

## Safety record

One private in-process TCP fixture family, all in ignored scratch under
`bench/out/nfs-contract117-critic/`. `environment-traps` was read before any process work.
Every daemon here is a test-server child of my own cargo invocation; **no daemon was signalled, no
mount was created, no filesystem was deleted**, and no unmount or `rm` of a mount path occurred at
any point in this review.

The only mutation I made was to two archive copies under my own ignored scratch, applied with an
assertion that the anchor line occurred exactly once, and I confirmed after the fact that the
worktree's `errors.rs` line 7 was still `Error::NotFound => nfsstat3::NFS3ERR_NOENT` and that the
tracked tree had zero changes. **The mutation was never restored in a branch, never committed, never
pushed, and never merged.** No checkout, reset, stash or branch change was performed.

Protected and untouched, verified at the end: shared daemon 15263 still ppid 1 and still started
`Sat Oct 3 20:44:29 2026`, its store, mount and socket unchanged; all 16 pool leases present with no
`treehouse` command run; the other workers' lanes (112, 116, 92, 79, 100, 102, 113, 114, 106, 118)
untouched; no installs, no sudo, no sysctl, no reboot, no runner or workflow change.

## Recommendation

**Merge.** The change is one test file, it removes a falsifiable-by-construction assertion, and it
replaces it with three assertions that I showed reject a real mutant through the same public caller
and the same fixture. It invents no atomicity guarantee, it does not touch production, and it does not
touch the #90 surface.

Follow-ups, none of them blocking: issue 122 for the GC tautology at `control.rs:75`, which is the
same defect class and is already tracked; and a note that the shared
`cowfs_nfs::is_listed` and `cowfs_daemon::mounts::is_mounted` remain fail-open for callers other
than the crash fixture, which I have now raised twice and is still unfixed.

Two process notes for the record, because both are the kind of thing that silently produces a wrong
review: my shared-target-directory mistake above, and the fact that adjacent `test result` lines in a
CI log belong to different binaries. Neither changed a verdict, but only because both were checked.

I did not merge this, and this report is not a published link: it is a file in the primary checkout.