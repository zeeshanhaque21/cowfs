# Issue #98: warm-base provenance is published durably

Task: #98, on top of the issue #17 namespace PR (#92).
Head at the time of writing: the commit that adds `crates/cowfs-daemon/src/base_meta.rs`.

## What was wrong

`base refresh` exited 0 and returned a `SnapshotInfo` carrying `repo`, `git_ref` and `commit`, and
wrote none of it down.

Both backends answered "is this a base?" from an in-memory set rebuilt on every open, and rebuilt
`BaseMeta` with all three fields `None`:

- `crates/cowfs-daemon/src/backend.rs`: `PathSnapshots::promote` and `CoreSnapshots::promote` inserted
  a name into a `BTreeSet`; `PathSnapshots::info` and `core_info` then built
  `BaseMeta { repo: None, git_ref: None, commit: None }`.
- `crates/cowfs-daemon/src/import.rs`: `base_refresh` set `info.base` on the value it was about to
  return, and returned it.

So `find_base` could never match on a commit, and the only way to see the commit was to read the
response of the call that created it. A caller that reconnected, or a daemon that restarted, was told
the base had no provenance.

Reproduced before the fix, real Linux, real FUSE mount, path backend, `bench/out/namespaces17-integration-repair/accept.log`:

```
refresh: seed published its base and exited 0
no base commit is recorded, so no warm base was published: {'pool_id': 'repo-65b7fe',
 'snapshot': 'repo-65b7fe-base', 'base_commit': None,
 'head_commit': '69f160e91f67dcf7a9ac73b94df1dc2a44f0ca97', 'fresh': False,
 'reason': 'no warm base repo-65b7fe-base for this repository'}
VERDICT: FAIL
```

## The fix

One durable record per base, owned by the daemon, at `<store>/.cowfs-base-meta/<name>/base.json`.

- Outside every snapshot tree, so no snapshot's contents, hash or Merkle root change and a clone never
  copies it. A name beginning with a dot is not a valid snapshot name, so the directory is invisible to
  snapshot listing on both backends.
- Written to a temporary file, fsynced, renamed, and the containing directory fsynced, so a reader
  never sees a partial record and the rename survives a power loss rather than only a process exit.
- Every field optional. A store written before provenance existed loads with the fields unknown, not
  absent: it is still a base, and it reports itself not fresh. An unreadable record fails the open
  rather than serving a store whose bases all look absent, because "all unknown" would let a refresh
  publish a second base under a name that already has one.

`Snapshots` gains `set_base_meta`. `base_refresh` promotes, records, then re-reads through
`create_meta` and reports what was stored, so the response can only say "published" if a later caller
reading the same store would agree. A write that fails fails the refresh; the tree is left in place
and reports itself not fresh, never falsely fresh.

`promote`, `rename` and `remove` keep the record in step with the namespace on both backends.

Scope: `Snapshots` and the two implementations in `backend.rs`, the publication call in `import.rs`,
and `base_meta.rs`. No change to `cowfs-meta`, `cowfs-core`, the store format, the queue, the control
server or any atomic-rename path. For the core backend the record lives in the store directory the
daemon already owns rather than in the core's redb snapshot table, because that table's schema belongs
to the #42 lane and changing it is a store-format migration. If provenance ever belongs in the core's
own metadata, that is the place to move it, and this is the one design decision here that may want
revisiting once that lane lands.

## Tests

`cargo test -p cowfs-daemon --lib`: 71 pass, 0 fail. Was 55 before this work and 62 after the
`base_meta` unit tests alone; #97's seven worktree regressions and the rest of the suite are unchanged
and still pass.

New, and what each is for:

| Test | What would break it |
|---|---|
| `a_record_survives_reopening_the_store` | the record was only in memory |
| `an_unwritten_base_is_unknown_rather_than_claimed` | "no record" read as "not a base" |
| `an_old_record_without_provenance_is_a_base_with_unknown_fields` | a missing field read as a claim |
| `an_unreadable_record_is_an_error_rather_than_an_empty_store` | a corrupt record read as an empty store |
| `the_record_lives_outside_every_snapshot_tree` | a clone copying the record, changing a hash |
| `an_invalid_snapshot_name_is_refused` | `../escape` reaching the filesystem |
| `removing_a_base_forgets_its_record` | a removed base still reported as a base after reopen |
| `a_core_base_keeps_its_provenance_across_a_reopen` | the core backend keeping provenance in memory |
| `a_path_base_keeps_its_provenance_across_a_reopen` | the same on the path backend |
| `a_promoted_base_with_no_provenance_reports_itself_unknown_not_fresh` | unknown reported as fresh |
| `provenance_follows_a_rename_and_is_forgotten_by_a_remove` | a rename losing provenance |
| `a_store_with_an_unreadable_base_record_is_refused` | a corrupt store being served |
| `a_provenance_write_that_fails_leaves_the_old_record_and_no_claim` | a failed write claiming success |
| `provenance_cannot_be_recorded_for_a_snapshot_that_does_not_exist` | provenance for a phantom snapshot |
| `a_refresh_is_still_a_published_base_after_the_daemon_reopens_the_store` | the defect, end to end |
| `a_refresh_that_cannot_record_its_provenance_fails_and_leaves_no_fresh_base` | a refresh reporting success it cannot back |

Both backends are exercised at runtime, not through a mock: the core tests open a real
`CoreBackend` over a temp store, reopen it, and read the base back through the trait.

## Real Linux acceptance

`scripts/namespaces17-treehouse-linux.sh` on moonscape, path backend, real FUSE mount, `base refresh`
through `cowfs-treehouse base refresh --canonical`. Exit 0. Raw log:
`bench/out/namespaces17-publication-repair/accept.log`.

```
warm base: the control API reports it published and fresh; snapshot repo-70d073-base
warm base: the reported commit is f69e7a8666495cc72558394fd3e04b17b4605609
warm base: one worktree, a clean tree, HEAD still f69e7a8666495cc72558394fd3e04b17b4605609
readback: the reopened daemon reports repo-70d073-base at the same commit f69e7a8666495cc72558394fd3e04b17b4605609
control: the same base, the same commit, and fresh=false because the repository moved
control: an unrefreshed repository has no commit and is not fresh
VERDICT: PASS
```

The commit `f69e7a86...` is the repository's own, taken by the fixture before the refresh, and the same
string comes back out of the store after a daemon that never saw the refresh in memory. Both
`base-status.log.commit` and `base-status-reopened.log.commit` hold it.

What the run asserts, and what each is for:

- `base refresh` exits 0 and `base status` reports a non-null commit and `fresh: true`. The
  postcondition, not an artifact.
- The refresh leaves the repository with one registered worktree, a clean tree and the same HEAD, so
  the record was written to the store and not into the caller's repository.
- After the daemon is shut down and the store remounted, a daemon that never saw the refresh reports
  the same base at the same commit.
- The two slots are cloned from the published warm base, not from the imported seed, and each holds
  the base's `main.rs`.
- Two fresh slots built at one canonical path are byte-identical; both do-nothing native controls at
  their own paths differ from theirs; the canonical artifacts embed the canonical path and the
  controls embed their own.
- The caller's mount table is unchanged and the canonical directory is empty outside every namespace.
- Counterexample 1: move the repository forward, and the same base reports `fresh: false` with its
  commit unchanged. Without this, a base that merely recorded a commit would look as good as one that
  is compared.
- Counterexample 2: a repository that was never refreshed reports `base_commit: null` and
  `fresh: false`, so no base is matched by root or by name.

Three assertions in that script were wrong about the product and had never been reached, because the
run always failed earlier at the postcondition. They are fixed in the same commit as the provenance
work, and the reasoning is in that commit message: `base refresh` builds in a leased slot but
publishes a fresh checkout of the ref, so the base holds committed source and no build artifact; the
readback loop named a snapshot `base` that does not exist; and `published_base` printed two lines into
a single-line capture.

## Not claimed

- The core backend at runtime over a mount. `base_refresh` refuses directory ingest on the core by
  design, so the core path for this task is covered by the runtime reopen tests above and not by the
  Linux run. No claim about the core under a real mount.
- `fsck`, crash durability or power-loss behaviour. The record is fsynced and renamed, which is a
  mechanism, not a measurement; no crash injection was run.
- Deduplication or warm-base build-time benefit. Nothing here measures any.
- btrfs, XFS, kernels older than this host's, or GitHub Actions. The measurement is one kernel on one
  host.
- The ETXTBSY flake. Not fixed, see below.

## The ETXTBSY flake in `crates/cowfs-treehouse/tests/canonical.rs`

Still open, still not worked around, and still not fixed. Five harness changes were measured before
this task and none held. Per the brief this got a spike instead of a sixth guess.

Probe: `scripts/namespaces17-etxtbsy-spike.rs`, run on the same host. Raw log:
`bench/out/namespaces17-publication-repair/etxtbsy-spike.log`. Linux 6.12.109+rpt-rpi-2712 aarch64,
1600 execs per row.

| Configuration | ETXTBSY |
|---|---|
| write then exec, `posix_spawn`, 8 threads, tmpfs | 40 / 1600 |
| write then exec, `posix_spawn`, 8 threads, ext4 | 73 / 1600 |
| write then exec, `fork` + `exec`, 8 threads | 1 / 1600 |
| exec only, no write at all, `posix_spawn`, 8 threads | 0 / 1600 |
| exec only, no write at all, `posix_spawn`, 1 thread | 0 / 400 |

It needs all three of a write to the file, concurrency, and the `posix_spawn` path glibc takes when
`std::process::Command` has nothing to intercept. It is not the filesystem, it is not the shebang and
it is not the dynamic loader: a statically linked binary with no interpreter and no libraries fails
the same way.

Two claims from the earlier rounds are withdrawn, and the test file's comment now says so. The write
handle was not ruled out: the probe mode labelled "no write in the loop" created a fresh directory and
wrote before every exec, so it never tested what it said. And the scan that reported nobody holding the
file was looking at the file under test, whose write handle the writing thread had already closed, so
it answered its own question. At the moment of each failure no process holds the file open and no
write-mode descriptor on the box is a library or an executable, so "somebody else is writing it" does
not explain it either.

What the evidence points at is the spawn call: `CLONE_VM|CLONE_VFORK` under concurrent spawns on this
kernel. The tests and the product's `run_build` both go through it, so this is not a fixture artefact.
That is a characterisation, not a mechanism, and nothing here establishes the kernel's reason, so no
fix is claimed and none is shipped. Establishing it would need kernel instrumentation this brief does
not permit. Status: BLOCK, root cause unconfirmed, not worked around.