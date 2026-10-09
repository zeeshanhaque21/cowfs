# Review: PR 92 metadata repair at 6929e63, against f1529cc

Reviewer: native critic 14, worktree `.treehouse/cowfs-7c1bf8/14/cowfs`, branch `review/linux-namespaces-17`, lease `0f3d1e5092d0bbec0544cd33ba163168`.

Subject: `6929e630d2e78547c758c8adc618ac76405f4588` (`docs(verification): what the review found, what changed, and what is still open`), code commit `eb17bc0` (`fix(daemon): a base record is never lost, and never claimed when it was`).
PR 92's head is exactly this SHA.

This is the canonical copy.
It lives in the main repository's primary checkout at `docs/reviews/linux-namespaces17-metadata-repair-final.md`, with a byte-identical mirror in the leased worktree.

My four earlier reports are preserved.
`docs/reviews/linux-namespaces17-publication-final.md` was copied verbatim from the lease into this primary checkout before this review, sha256 `2aeeca9ff657db950853e57c7e608716c0d96fdd55b49b0b36a4db9ddb12de8b`, verified with `cmp`.
The other three remain in the lease: `linux-namespaces17-helper-final.md` at `375b005`, `linux-namespaces17-integration-final.md` at `7faac1be`, `linux-namespaces17-refresh-repair.md` at `e243cb1`.
Remote private areas `cowfs-ns17crit`, `cowfs-ns17-int`, `cowfs-pub` and this round's `cowfs-p98` are all verified present and unchanged where they should be.

Coordination: `docs/ready-wave-dispatch.md` is not in my branch, so I read it from the main checkout, read-only.
It records eleven ready workers under `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/`, a different pool from my build-train lease, and it forbids `cowfs-meta` and `import.rs` being touched by the wrong lane.
`crates/cowfs-meta/src/tx.rs` is byte-identical at `f1529cc` and at this head, sha256 `5cafb0cc2e10f1e2cde879258ba96c6a41308bd8faf06b402298824aeba18688`, and is not in this diff, so I did not edit it.
`crates/cowfs-daemon/src/import.rs` is in this diff, but only its test module changed.

**One deviation from my brief, stated up front.**
The brief assigned me `/home/moonscape/cowfs-ready-wave/task-ns17-metadata-review/**` and also said never to write ready-wave pools from the original critic.
Those conflict, so I resolved toward the stricter rule and used my own private root, `/home/moonscape/cowfs-p98`, outside the ready-wave pool entirely.
The ready-wave pool was never written to by me.
It now holds 5.5 GiB from other workers, and its two live daemons on `task-g4` and `task-g5` and the host's two cowfs mounts and three FUSE connections were left untouched.

Every heavy Linux step ran as one foreground command through `/home/moonscape/cowfs-ready-wave/linux-heavy.lock` by the documented recipe, 600-second bounded wait, no polling.

## Verdicts

**P-1, `remove`: PASS.**
Both failure branches measured on a real Linux daemon, and in both the live process and a daemon restarted on the identical store give the identical answer.
The error is reported, never swallowed, and the in-memory map is reconciled against what the store actually holds rather than kept or dropped unconditionally.

**P-2, `rename`: PASS.**
Destination collision refused and the destination preserved byte-for-byte.
An unwritable destination leaves the source byte-identical and creates nothing at the destination.
A tree rename that fails *after* the record has moved rolls the record back, reconstituting the source durably, and the original error is what the caller sees.
I found a deterministic injection for that last case rather than leaving it to code reading.

**P-3, temporary files and locking: PASS.**
Per-call unique temporary name, `create_new`, and the store's own mutex held from the map read through the atomic publish to the cache update.
Zero stray temporary files measured across everything this round created.

**P-6, re-runnable in place: PASS.**
Two runs, back to back, in one checkout, both `EXIT=0`, in different attempt directories, with all 126 of the first run's files identical afterwards in size, mtime and sha256.
This is the third head on which I reported the in-place failure, and it is genuinely fixed.

**P-7, name and symlink handling: PASS, with one code-read caveat.**
Empty, `.`, `..` and slash-bearing names are refused on both backends through the public API, and the two-tier validator split is deliberate and documented.
A symlinked record directory is not read through, measured.
The caveat is that the delete path relies on `remove_dir_all` not following symlinks rather than refusing explicitly, which is code-read only.

**P-4 and P-5, the doc overclaim: PASS, corrected exactly.**
The path shape is now stated correctly as `<name>/base.json`, and the "invisible to the mount root" claim is withdrawn and replaced with the measured behaviour plus a reason.
I confirmed the withdrawal against my own earlier measurement of the same leak.

**Orphan records, the one failure that must never happen: PASS on both backends.**
A record planted directly in the store with a real commit and no snapshot behind it did not attach to the next snapshot of that name, live or after a reopen, on either backend.

**One new defect, and it is a BLOCK: on the Core backend a refused `snapshot create` destroys a live base's provenance.**

`CoreSnapshots::create` clears the base record *before* the core rejects a duplicate name, so a call the API correctly refuses still performs the destructive part.
`PathSnapshots::create` refuses the taken name first and keeps its record.
Measured on both backends, on a real daemon, live and after a reopen.

This is introduced by this head's own orphan fix, it is reachable with one command, and it is the backend-asymmetric case the change was written to prevent.

**ETXTBSY: still a separate merge BLOCK, carried.**
Not re-run this round, by instruction and because the scoped proof was already accepted and the host is contended.
Nothing about it is fixed by anything above.

Source verdict and merge verdict stay separate: the repair work is correct and I would merge it once the Core `create` defect is fixed, and the branch is not merge-ready while the flake stands.

## Binding

`git archive` of the exact head into my private remote root, five files matched:

| File | sha256 |
|---|---|
| `crates/cowfs-daemon/src/base_meta.rs` | `47917c4975602b40696222c9221ce2901195be6fb2aeeaee162f1f0aa035d36a` |
| `crates/cowfs-daemon/src/backend.rs` | `a08d1855bec39cbd250612ce503d6fd6a03f968f6facc9c370efb427d0cee6de` |
| `crates/cowfs-daemon/src/import.rs` | `914b7732407bdaca3931593ab27e62fd6372e941591d20c5da50a7b49078eec5` |
| `scripts/namespaces17-treehouse-linux.sh` | `1ab9e2e67869011f3de154cbf63a02d1c31367d092c8b4132f493bb9290d3792` |
| `docs/verification/base-provenance98.md` | `a64116eb334b3e8f779f7b0faabf01703226e54fca3faf4d1fafc9186e053fcc` |

The canonical `docs/verification/base-provenance98.md` in the main checkout hashes to `a64116eb334b3e8f779f7b0faabf01703226e54fca3faf4d1fafc9186e053fcc`, which is exactly the blob at this head, so the published document and the reviewed document are the same bytes.

Host: `moonscape`, `moonscape@192.168.68.119`, Debian aarch64, kernel `6.12.109+rpt-rpi-2712`, uid 1000, git 2.39.5, rustc 1.95.0, clippy 0.1.95.

The builder measured on macOS with rustc 1.99.0 and clippy 0.1.99.0.
That difference is stated wherever it matters below, and I do not reconcile the two toolchains.

## Part 1: the deliverable, run twice

The acceptance script, unmodified, at this head, from the repository root, twice in the same checkout:

```
S1 EXIT=0  VERDICT: PASS  attempt=a1005T012656-1126444
S2 EXIT=0  VERDICT: PASS  attempt=a1005T012836-1132405
```

Each run published its own warm base at its own commit, and each was re-read by a daemon that never saw the refresh in memory:

| | S1 | S2 |
|---|---|---|
| published commit | `c840428b43c5bc3adfadb83a43113723245abc9a` | `4a1c3d9b098b5625a7765eb4bf9d50d6942309c7` |
| same commit after reopen | yes | yes |
| base snapshot | `repo-525969-base` | `repo-2e7fca-base` |
| worktrees after refresh | 1, clean tree, HEAD unchanged | 1, clean tree, HEAD unchanged |
| record on disk | commit matches, `promoted: true` | commit matches, `promoted: true` |
| socket path | 73 bytes of the 107 a Unix socket allows | same |

Isolation, which is the actual point of P-6:

```
S1 UNCHANGED: every file's size, mtime and sha256 identical after S2
S1 and S2 are different directories: YES
both still present: S1=yes S2=yes
S1 fingerprint: 126 files
```

The repository, the store, the mount point, the socket, the daemon, the treehouse HOME, the canonical directory and the fixture are all inside the attempt directory, named `a<mmddThhmmss>-<pid>`.
The script refuses to run inside an attempt that already exists, and refuses to recreate its fixture repository or its control repository, so a name collision is a refusal rather than an overwrite.
`CARGO_TARGET_DIR` is deliberately shared across attempts under `bench/out/ns17/target` and documented as a build cache rather than a fixture.

One narrow gap in the socket check, worth naming because the check exists precisely to prevent the failure it prevents.
`${#sock}` counts characters, and the kernel limit of 107 is a byte count.
For the all-ASCII paths in this project the two agree, and the measured length was 73, so nothing is currently at risk.
The threshold of 100 is correctly conservative against the cited 107.
If a repository path ever contained a multi-byte character, the check would undercount and could admit a path the kernel would refuse.
One `wc -c` would make it exact.

## Part 2: the Core `create` defect

`CoreSnapshots::create`, `backend.rs:587-602`:

```rust
fn create(&self, name: &str, from: Option<&str>) -> io::Result<SnapshotInfo> {
    // A new snapshot is never a base, so any record left under this name by a snapshot that has
    // since gone is cleared before it exists.
    self.bases.remove(name)?;
    self.with(|c| { ... c.create_snapshot(name) ... })
}
```

`PathSnapshots::create`, `backend.rs:805-810`, has the order the other way round:

```rust
if self.exists(name) {
    return Err(io::Error::new(io::ErrorKind::AlreadyExists, "name is taken"));
}
// A new snapshot is never a base, so a record left under this name ...
self.bases.remove(name)?;
```

Neither `handler.rs::snapshot_create` at line 246 nor the control server at line 954 pre-checks existence; the server only validates the name's shape.
So on Core the record deletion happens first and the duplicate rejection second.

Measured, Core backend, real daemon:

```
create warm EXIT=0
promote warm EXIT=0 (warm[base])
record on disk: {"repo":null,"git_ref":null,"commit":null,"promoted":true}

listing BEFORE the duplicate create:
  name='warm' base={"commit": null, "git_ref": null, "repo": null}

create warm AGAIN EXIT=1
  cowfs: cannot create snapshot "warm": snapshot exists

listing AFTER the refused duplicate create:
  name='warm' base=None
record on disk: GONE

listing after a reopen:
  name='warm' base=None
```

Measured, Path backend, the identical sequence:

```
create warm AGAIN EXIT=1
  cowfs: cannot create snapshot "warm": name is taken
warm AFTER the refused duplicate create:
  base={"commit": null, "git_ref": null, "repo": null}
record on disk: {"repo":null,"git_ref":null,"commit":null,"promoted":true}
```

So on Core the snapshot survives and its base annotation is destroyed, by a call the API correctly refused, and the loss is durable because it is on disk before the daemon restarts.
On Path the same sequence loses nothing.

The direction is safe for a reader, because a lost record reports "no warm base" rather than a forged fresh.
The cost is that a real, correct, physically present base silently stops being a base.
Any operator or tool that runs `snapshot create` twice against a name, or retries a create that it did not check the result of, un-publishes a warm base on the Core backend.

Minimal fix shape: give `CoreSnapshots::create` the same existence pre-check `PathSnapshots::create` has, so the record is only cleared for a name that is genuinely free.
One `if self.info(name).is_ok()` or equivalent at the top, matching the Path wording and error kind.

This is a regression introduced at this head, not a pre-existing bug.
At `f1529cc` neither backend's `create` touched the record at all.

## Part 3: P-1, both failure branches, measured

The property that matters is not that each branch behaves, but that a live process and a restarted one cannot disagree about it.

**Branch A, the record survives.**
The base's own record directory made read-only, so nothing can be removed.

```
snapshot rm EXIT=1
  cowfs: cannot remove "warm": cannot remove the base record
  .../R3a/store/.cowfs-base-meta/warm: Permission denied (os error 13)
record file still there: YES
LIVE, warm: gone (no snapshot named warm)
REOPENED, warm: gone (no snapshot named warm)
AGREE: live and reopened give the identical listing
```

**Branch B, the record file is gone but its directory cannot be removed.**
The metadata root made read-only instead, so unlinking inside the child still works while removing the child does not, which is exactly the partial state the code comment describes.

```
snapshot rm EXIT=1
  cowfs: cannot remove the base record ...: Permission denied (os error 13)
the record FILE now: GONE
the record DIR now: still-there
LIVE:      (no snapshot named warm)
REOPENED:  (no snapshot named warm)
AGREE: live and reopened give the identical listing
```

In branch B the store is left with an empty orphan directory, and `open` skips a directory with no `base.json`, which is why the restarted daemon does not resurrect anything.

This is the fix for the divergence I reported last round, where a live daemon said "no warm base" while a restarted one named the old commit.
At this head both branches agree, and the reconciliation is conditional on the file actually existing rather than assumed in either direction.
The error is also reported instead of discarded, and `remove` returns `io::Result<()>` so a caller can see it.

## Part 4: P-2, three cases, all measured

**Destination already holds a different record.**
The Path pre-check refuses first here, so the store's `AlreadyExists` guard is not the code that answers; that path is covered by the unit test `a_rename_never_overwrites_a_different_destination_record`.
What the API shows is that nothing is overwritten:

```
rename src dst EXIT=1   cowfs: cannot rename "src" to "dst": name is taken
dst record unchanged: YES
src record unchanged: YES
both snapshot trees still present: src=yes dst=yes
```

The guard itself compares the whole `Record`, whose four fields are `repo`, `git_ref`, `commit` and `promoted`, all `Option`, so a difference in any of them refuses.
That answers the concern about a silent "different meta" comparison losing a field: `Record` derives `PartialEq` over all four, and `promoted` is included, so a record that differs only in promotion state is still a collision.

**Destination cannot be written.**

```
rename warmer warmest EXIT=1
  cowfs: cannot rename "warmer" to "warmest": cannot create the record directory
  .../.cowfs-base-meta/warmest: Permission denied (os error 13)
warmer record byte-identical: YES
no record created for warmest: YES
warmer's tree survived: yes
warmest's tree absent: yes
```

**Tree rename fails after the record has moved, so the rollback must run.**
This was code-read only until I found a deterministic injection.
A plain file placed at the destination is not a snapshot directory, so it passes the taken-check, the record moves, and then `std::fs::rename` on the tree fails with `ENOTDIR`:

```
planted a plain file at the destination
rename src blocker EXIT=1
  cowfs: cannot rename "src" to "blocker": Not a directory (os error 20)
src record rolled back and byte-identical: YES
no record left under blocker: YES
src tree still present: yes
src still reports its commit: base={"commit": null, "git_ref": null, "repo": null}
after a reopen: base={"commit": null, "git_ref": null, "repo": null}
records on disk now: dst, src, warmer  (no blocker, no warmest)
```

The caller's error is the original `Not a directory`, not a success and not a rollback message, and the source record is back and byte-identical.
`rollback_base` composes the two only when the rollback itself fails, so a failed rollback would report both causes rather than discard the original.

The residual risk the brief asked about is real but bounded, and I can now say what it is.
If the record moves, the tree rename fails, *and* the rollback's own removal of the destination record fails, two records exist and the map says so, which is what a reopened process reads.
That is safe against a forged fresh base for a specific reason: `create` now clears an orphan record before the snapshot exists, which I verified on both backends.
So a stray record under a name with no snapshot cannot attach itself to whatever takes that name next.

## Part 5: P-3, P-7, and the orphan property

**P-3.**
The temporary name is now `{FILE}.tmp{pid}.{TEMP_SEQ.fetch_add(1, Relaxed)}`, opened with `create_new(true)` in a retry loop, and the store's mutex is held across the whole of `write_locked` and `remove_locked`, so the map read, the file publish and the cache update cannot interleave with another thread.
Measured: zero `base.json.tmp*` files anywhere under my private root after this round, which created and removed many records.
The concurrency tests, bounded at three iterations each because other workers are on this host: the two `concurrent_writes` tests pass three times out of three, and the fifteen `base_meta` tests pass three times out of three.
I did not run the 200-iteration batch and do not claim it.

**P-7.**
Through the public API, on both backends:

| name | Path | Core |
|---|---|---|
| `""` | exit 1, `invalid snapshot name "": empty` | same |
| `.` | exit 1, `must not start with a dot` | same |
| `..` | exit 1, `must not start with a dot` | same |
| `a/b` | exit 1, `must not contain a slash` | same |
| `a b` | exit 0, accepted | exit 0, accepted |

A space is not illegal under the snapshot-name rule, so accepting it is correct rather than a miss.
A NUL cannot be expressed through argv; `check_private_name` tests `name.contains('\0')` in source at line 90, and the unit test `remove_refuses_a_name_that_could_leave_the_metadata_root` covers the function directly.

The two-tier split is the right design and is documented at lines 76-81: a real base name must satisfy `validate_snapshot_name`, while `remove` and `rename`'s source name only have to be one safe path component, because `import` creates and removes private `.cowfs-import-<name>` staging snapshots that are deliberately not valid snapshot names.
Naming those two cases explicitly is what keeps the weaker check from looking like an oversight.

**Symlinked record directory.** A record directory replaced by a symlink pointing outside the store, with a decoy `base.json` in the target:
the daemon started, reported `base=None` for that name, and did not adopt or rewrite the decoy.
`open` skips it because `DirEntry::file_type` does not follow symlinks.
`write_locked` refuses it explicitly through `no_symlinked_dir`.
`remove_locked` does not call `no_symlinked_dir`; it relies on `remove_dir_all` not traversing a symlink, which on Linux unlinks the link itself.
That is safe in effect but it is not the explicit refusal the brief asked for, and it is code-read only.

**Orphan record, both backends.**
A record for `warm` carrying `commit: deadbeef...` and a real repository path, written straight into the store with no snapshot behind it:

```
path: create warm EXIT=0, then base=None, record GONE, after reopen base=None
core: create warm EXIT=0, then base=None, record GONE, after reopen base=None
```

A commit that was never built from anything is never adopted, on either backend, live or after a reopen.
This is the failure the whole design exists to prevent and it does not occur.

## Part 6: serialization, honestly scoped

The brief asked whether promote, rename and remove are serialized consistently against the shared backend.
The answer is partly, and less than the change implies.

`BaseMetaStore.records` is a mutex held across every record operation, so record-level operations are serialised within a process.
That much I measured, through the clean temporary-file sweep and the concurrency tests.

Above that layer, the production handler does not serialise namespace operations per name.
`handler.rs::remove` takes a `HolderGuard` lock, but its own comment says that is for the holder check, not for change ordering.
`snapshot_rename` at line 288 and `snapshot_promote` take no lock at all.
The `lock_for` helper that does take a per-name lock lives inside `WithHolders`, a test wrapper used by the conformance suite, not in the production path.

So the store's lock guarantees that a record's bytes and its cache entry agree.
It does not make a record move and a tree move one atomic step, and a concurrent `snapshot rename` and `snapshot promote` on one base can still interleave at the namespace layer.
That is pre-existing, not introduced here, and the brief explicitly rules out a broad transaction rewrite, so I am recording it as a limit rather than a defect of this change.
It does mean the word "atomic" in the module documentation should be read as atomic *per record file*, which is what the code does.

## Part 7: counts on this host and toolchain

Each exit captured on its own line, no pipeline in a status position.
These are Linux numbers on rustc 1.95.0, reported as mine and not as a correction of the builder's macOS numbers on 1.99.0.

| Suite | Result | Exit |
|---|---|---|
| `cargo test -p cowfs-daemon`, all targets | 81 lib passed, 0 main, 5 ignored in `end_to_end`, 0 doc | 0 |
| `cargo test -p cowfs-daemon --lib base_meta::` | 15 passed, 66 filtered out, three iterations | 0 |
| `cargo test -p cowfs-daemon --lib concurrent_writes` | 2 passed, 79 filtered out, three iterations | 0 |
| `cargo test -p cowfs-treehouse --test canonical`, Linux | `running 13 tests`, 13 passed | 0 |
| `python3 -m unittest discover -s bench -p test_namespaces.py`, Linux | `Ran 17 tests`, `OK (skipped=1)` | 0 |
| `cargo fmt --all --check` | clean | 0 |
| `cargo clippy -p cowfs-treehouse --all-targets -- -D warnings` | clean | 0 |
| `cargo clippy -p cowfs-daemon --all-targets -- -D warnings` | fails in a dependency | 101 |
| `cargo clippy --workspace --all-targets -- -D warnings` | one `collapsible_match` | 101 |

`cowfs-daemon` lib by module on this host: `backend` 18, `base_meta` 15, `import` 14, `handler` 14, `exports` 13, `mounts` 3, `holders` 2, `daemon` 2, totalling 81.

Against the builder's macOS figure of 82, the totals differ by one and the per-module split differs in two places: `backend` 18 here against 17 there, and `holders` 2 here against 3 there.
The builder also lists a separate "1 store probe".
I did not reconcile those three differences and I am not going to guess at them; a platform-conditional test in `backend` or `holders` is the obvious candidate, and it stays a hypothesis until someone names the test.
What is not in dispute is the delta: the builder measured 55 to 71 over this interval and I measured 54 to 70 at the two previous heads, so both agree the change adds 16 to the same interval with the same one-test baseline offset, and the builder's own document says it did not reconcile that offset either.

`canonical.rs` cfg breakdown, computed from source, with the platform-gated tests named:

```
total #[test]   : 14
linux-only      : 4   the_argv_keeps_the_build_command_as_one_element
                      the_canonical_path_is_never_pasted_into_the_command_string
                      a_refused_namespace_is_unsupported_and_the_build_does_not_run
                      a_working_namespace_lets_a_failing_build_stay_a_failure
not-linux-only  : 1   off_linux_a_valid_canonical_pair_is_unsupported_and_never_ignored
both platforms : 9
=> Linux runs 13, macOS runs 10
```

Those match both my earlier Linux measurement and the builder's macOS run of 10, and the two platform-expected Python figures match too: 17 tests with 1 skipped on Linux, 17 with 10 skipped on macOS.

**The clippy discrepancy is version-dependent and unresolved, and both observations stand.**
On clippy 0.1.95 here, `--workspace` exits 101 with `collapsible_match` at `crates/cowfs-meta/src/tx.rs:314:21`.
On clippy 0.1.99.0 on the builder's machine, the same command exits 0.
The file is byte-identical at `f1529cc` and at this head, sha256 `5cafb0cc2e10f1e2cde879258ba96c6a41308bd8faf06b402298824aeba18688`, and is not in this diff, and the shape the lint points at is still present at `tx.rs:312`.
So this is a lint that fires on one clippy and not the other, on a file nobody here changed.
The builder reported that as a discrepancy to resolve rather than as a fixed gate, which is the right handling, and I am not claiming the gate is green.
It needs one run of the same clippy version over the same commit to settle, which is a two-minute job for whoever owns that file.

## Part 8: CI

One read of the exact head, no polling, no dispatch, no rerun.

```
6929e63 check-runs: linux-fuse success, check (macos-latest) success, check (ubuntu-latest) success
total: 3
```

PR 92's head is `6929e630d2e78547c758c8adc618ac76405f4588`, the SHA I reviewed, state OPEN.
Green CI does not establish anything about ETXTBSY: the flake has failed a green-source run before, and a single green run at a rate that has measured between 1 in 12 and 4 in 24 for me is not evidence it is gone.
The brief also asked me not to infer a private-Linux isolation proof from a green matrix, and I am not: my Linux evidence is the direct measurement in this report.

## ETXTBSY, carried

Not re-run, not retried, not ignored, not serialised, no wider bound attempted, and no kernel cause claimed.
My last measured figures stand: 1 failure in 12 full-binary runs at 8 threads, and 0 in 6 at one thread, both on this host, both too small to conclude that serialising helps.
The characterisation in the repository document is a better characterisation than mine and I do not dispute it.

One correction I do want to keep on the record, because the previous round's report of mine contained a misleading literal.
There is no explicit `posix_spawn` call in `crates/cowfs-treehouse/src/mode_b.rs`; it builds the helper with `std::process::Command`.
On Linux, Rust's `Command::spawn` uses `posix_spawn(3)` when it can avoid a fork, so the tests and production share a spawn path, but that is a statement about the standard library and not about that source.
That is the same correction the builder's document now makes, and it is the right one.

Status: merge BLOCK, root cause unconfirmed, independent of everything above.

## What is not proven at this head

- No Core-backend warm-base publication. `base_refresh` refuses directory ingest on Core by design; I exercised only create, promote, create-again, rename and remove there.
- No directory-ingest acceptance on Core, and not the 15/16 gate.
- No crash injection and no power-loss measurement. The record is fsynced and renamed, which is a mechanism, not a measurement.
- Removal is not crash-durable. `remove_locked` fsyncs nothing after the unlink, while `remove`'s own doc comment says "Forgets `name`, durably, before this returns". `write_locked` fsyncs the file and its directory; the removal path does not fsync the parent, so a crash could resurrect a record on disk that the live map has already dropped. The builder's document does not claim crash durability for removal, so this is a doc-comment overclaim rather than a broken guarantee, but it is the one asymmetry I found in an otherwise careful lifecycle.
- No concurrency measurement above the store layer. The `snapshot_rename` and `snapshot_promote` interleaving in Part 6 is code-read.
- No rollback-failure measurement. I proved the rollback runs and succeeds; I did not force the rollback itself to fail.
- The `AlreadyExists` guard inside `BaseMetaStore::rename` was not reached through the public API on Path, because Path's pre-check answers first. It is covered by unit test, not by my runtime gate.
- No 200-iteration concurrency batch, and no ETXTBSY spike re-run.
- No `fsck`, no deduplication, no build-time benefit, nothing about btrfs, XFS, other kernels or other architectures.
- No leased-slot result. moonscape has no `treehouse` binary, so `--slot` is used, the same `run_build` call site with a different slot provider.
- No macOS run by me at this head. Every Linux figure above is Linux; the only macOS figures in this report are the builder's, attributed as such.
- shellcheck is not installed on moonscape and I was not permitted to install it, so no verdict from it. `sh -n` and `dash -n` pass on all five of my probe scripts and on both project scripts.
- I did not review anything after `6929e63`.

## Minimal defects, no fixes applied

No source or test edits, no commit, no push, no merge, no rebase, no force, no lease return.

1. **The Core `create` defect**, `backend.rs:587-592`. Add the existence pre-check that `PathSnapshots::create` already has, so the orphan record is cleared only for a name that is genuinely free. This is the only merge-blocking defect I found.
2. **`remove_locked` durability**, `base_meta.rs:333-364`. Either fsync the parent directory after a successful `remove_dir_all`, or stop calling the operation durable in `remove`'s doc comment at line 192. Pick one and make them agree.
3. **`remove_locked` symlink handling**, `base_meta.rs:338`. Call `no_symlinked_dir` before `remove_dir_all` so the delete path refuses explicitly rather than relying on `remove_dir_all`'s symlink semantics.
4. **The socket length check**, `scripts/namespaces17-treehouse-linux.sh`. `${#sock}` counts characters and the kernel limit is in bytes. One `wc -c` makes the guard exact.
5. **The clippy version discrepancy**, `crates/cowfs-meta/src/tx.rs:312`. Not this PR's file and not mine to edit. Settle it by running one clippy version over one commit, then either fix the shape or record why the lint does not fire.
6. **ETXTBSY**, not fixed here by instruction. It stays a merge BLOCK on its own.
7. **Keep issue 17 open.** A passing Path-backend integration and correct metadata lifecycle are real progress and are not the mode (b) warm-base acceptance: Core publication, the ETXTBSY flake, and the workspace clippy gate all remain.
8. The canonical `base-provenance98.md` is accurate as written, including its refusal to claim the clippy gate as fixed and its refusal to claim a `posix_spawn` call that is not in the source. I found no claim in it that is contradicted by my measurements.

## Reproduction

Mac, leased worktree:

```sh
cd /Users/zeeshanhaque/Projects/cowfs/.treehouse-build-train/.treehouse/cowfs-7c1bf8/14/cowfs
git rev-parse HEAD                                   # 6929e630d2e78547c758c8adc618ac76405f4588
git diff --stat f1529cc..HEAD
```

Linux, my own private root, every heavy step through the wave lock:

```sh
git archive --format=tar 6929e63 > src-6929e63.tar
scp src-6929e63.tar moonscape@192.168.68.119:/home/moonscape/cowfs-p98/
ssh moonscape@192.168.68.119 \
  'mkdir -p ~/cowfs-p98/repo && cd ~/cowfs-p98/repo && tar xf ../src-6929e63.tar \
   && sha256sum crates/cowfs-daemon/src/base_meta.rs crates/cowfs-daemon/src/backend.rs \
                crates/cowfs-daemon/src/import.rs scripts/namespaces17-treehouse-linux.sh \
                docs/verification/base-provenance98.md \
   && ./scripts/namespaces17-treehouse-linux.sh ~/cowfs-p98/repo; echo EXIT=$? \
   && ./scripts/namespaces17-treehouse-linux.sh ~/cowfs-p98/repo; echo EXIT=$?'
```

Expected, and what I measured: both `EXIT=0`, both `VERDICT: PASS`, two different attempt directories, and the first run's 126 files unchanged.

My probes and evidence, all under the lease at `bench/out/namespaces17-p98-repair-critic/`, which is gitignored:

```
src-6929e63.tar          the exact tree I reviewed
r1-two-runs.sh           the deliverable twice, with a 126-file fingerprint of the first run
r2-r3-r4.sh              the Core create contrast, P-1 branch A and branch B, P-2 all three cases
r5-r6.sh                 the Core finding via the API, name rejection, symlinked record dir,
                         orphan records on both backends, the bounded P-3 sweep
final-counts.sh          counts, fmt, clippy, the canonical.rs cfg breakdown

evidence-r1.txt           S1 EXIT=0, S2 EXIT=0, S1 UNCHANGED across 126 files
evidence-r2-r4.txt        Core base=None after a refused create; Path keeps its record;
                          both P-1 branches AGREE; all three P-2 cases
evidence-r5-r6.txt        the Core finding confirmed of the API, name rejection both backends,
                          symlink not read through, orphan cleared both backends, 0 temp files
evidence-final-counts.txt 81 lib, 15 base_meta, 13 canonical, 17 python, fmt 0, clippy 101
```

Cleanup: no process of mine and no mount of mine remain.
Two cargo `target` directories under my own private root were removed after confirming each held only `CACHEDIR.TAG debug tmp`, for 2.7 GiB reclaimed.
My root is 91 MiB with both attempt directories, every log and every fingerprint kept.
All four older private areas are verified present: `cowfs-pub` 208 MiB, `cowfs-ns17-int` 91 MiB, `cowfs-ns17crit` 31 MiB, and the round-1 artifact still `82f5af53d1dd8f569284f481c3f29b5ee3972d3fc422e737518015dea48db2d4`.
Mac-side owned growth this round is 5.7 MiB, well inside the 8 GiB allowance, and the Mac had 392 GiB free when I finished.

Four mistakes of my own this round, reported rather than left for someone else.
I resolved the conflicting path instruction by departing from the literal assignment and saying so at the top rather than writing into a pool I had also been told not to write into.
My R2 probe checked for a Core snapshot with a Path-backend directory test, which is meaningless on a Core store and briefly printed a false "the snapshot itself still exists: NO"; R5a re-asked the same question of the API and is the version I stand behind.
My R1 script picked the second attempt directory with `sort | tail -1`, which matched the shared `target` cache directory rather than the attempt; the per-run reporting in the same output used the correct glob and the logs name the real directories, so nothing downstream depends on the wrong label.
And I re-verified that the socket limit is reported as 107 in the script while the comparison is against 100, which is conservative and correct, before noticing that the measurement itself is in characters rather than bytes.
No package installs, no sudo, no sysctl, no reboot, no device formatting, no workflow dispatch, no rerun, no polling, and every signal was a single ownership-verified pid, never a group and never a `pkill`.