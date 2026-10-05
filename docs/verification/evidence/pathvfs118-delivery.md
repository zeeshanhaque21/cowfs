# Delivery of the #118 test repair to current main

Refs #118, with #120 named as the unchanged open question.
This is a delivery record, not new diagnosis.
The diagnosis and the repair report it delivers are immutable and unchanged:

| document | sha256 |
| --- | --- |
| `docs/verification/evidence/pathvfs118-diagnosis.md` | `fc73a654c8ee12b73d28cd4e55eec5c425f6e27e063dc7137cb7689bfe4a55c1` |
| `docs/verification/evidence/pathvfs118-test-repair.md` | `7eb5b1dc0e8320dbf6d97627e3b6857e38d3ccef84d3a301e0fadce402a78d03` |

`pathvfs118-test-repair.md` says the patch "is not applied anywhere" and describes the coordinator as
the deliverer.
That statement was true when it was written and stays true as history.
The patch is applied now, on the branch below, and this document is the current fact.
The repair report was not rewritten to say otherwise.

| | |
| --- | --- |
| PR | https://github.com/zeeshanhaque21/cowfs/pull/131 |
| branch | `test/pathvfs-stamp-precondition-118` |
| test commit | `78c5db55795abcf903f46106a6ddcdb57df1ec0b`, `crates/cowfs-vfs-path/src/tests.rs` only, +51/-1 |
| source diff | this test commit alone; the branch carries one further commit that adds only this delivery record |
| base | `main` at `00065ce75dcd554e1fb4bb084d1c70b2e2a21a87` |
| applied patch artifact | `bench/out/pathvfs118/repair/pathvfs118-test-repair.patch`, sha256 `efcd6f4e7e83ec0d255700d39907e8c2004cc3ef0d25c43d314b9b2dc2e766b5`, non-empty, hash as expected |
| source archive for the Linux run | `bench/out/pathvfs118-delivery/src-patched-78c5db5.tar.gz`, sha256 `019ba52a14e068ffebc5e4be955bb18b24d3143da0583185c2fe3911b3bca859` |

## Why the prepared patch applied with no adaptation

The patch was written against `951045fca4823611e196eda75db0c977a46d2c77`.
`main` moved on to `00065ce75dcd554e1fb4bb084d1c70b2e2a21a87`, with `951045f` an ancestor of it.
The three responsible seams did not change across that range, so the patch's exact contexts were
still present and no narrow adaptation was needed:

| file | blob at `951045f` | blob at `00065ce` | changed? |
| --- | --- | --- | --- |
| `crates/cowfs-vfs-path/src/tests.rs` | `90de21aa99f22c1d75f6f45f571ff1d33087597b` | `90de21aa99f22c1d75f6f45f571ff1d33087597b` | no |
| `crates/cowfs-vfs-path/src/table.rs` | `d61ca4226accb9db1bd15d739c0ccda43d8276d0` | `d61ca4226accb9db1bd15d739c0ccda43d8276d0` | no |
| `crates/cowfs-vfs-path/src/sys.rs` | `e94ebf03015fb557c4d953e4215382f092babcff` | `e94ebf03015fb557c4d953e4215382f092babcff` | no |

`git apply --check` passed and `git apply` reported `Applied patch crates/cowfs-vfs-path/src/tests.rs cleanly`.
No new helper, no new type, no changed approach.
The existing six-field stamp tuple is `(i64, i64, i64, i64, u64, u64)`, matching `table.rs`'s own key.

`Cargo.lock` did change between the two revisions (`22371dd9b450` to `64ccfc6ec32f`) when
`cowfs-snapname` landed.
That is unrelated to this test file and every command below ran with `--locked` against the tree's own
lock, so it is recorded rather than glossed over.

## Proof on the patched actual source, this delivery

The single test was run first, on the patched real source, on both hosts, before any suite.
Both are the actual patched source, not a re-derived variant.

### macOS, APFS

```
$ cargo test --locked -p cowfs-vfs-path --lib \
    tests::readdir_sees_a_name_created_outside_the_vfs_while_a_listing_is_paged -- --exact --nocapture
PRECONDITION stamp_before=(1791217778, 702781627, 1791217778, 702781627, 192, 6) stamp_after=(1791131378, 0, 1791217778, 703390838, 192, 6)
test tests::readdir_sees_a_name_created_outside_the_vfs_while_a_listing_is_paged ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 32 filtered out; finished in 0.01s
```

Exit 0, `1 passed`, `32 filtered out`.
The observed stamp changed: mtime moved back by exactly 86400s and the nanosecond field went to zero,
which is what an explicit `futimens` produces.
`size` was `192` before and `192` after, so on APFS too a create and a removal net to the same entry
count and only a timestamp distinguishes them.
The expected entries assertion was reached and the resumed listing produced `[a, b, c, d, e]`.

### Linux, moonscape, the host family the original failure reproduces

`Linux 6.12.109+rpt-rpi-2712 aarch64`, rustc 1.95.0, cargo 1.95.0.
Bounded root `/home/moonscape/cowfs-ready-wave/task-pathvfs118-delivery/`, `TMPDIR` inside it on
ext2/ext3, `linux-heavy.lock` held for the run.
A fresh archive of the branch head was extracted, so no copied `target` directory mtimes were seeded,
and `CARGO_TARGET_DIR` was isolated.

```
archive sha256: 019ba52a14e068ffebc5e4be955bb18b24d3143da0583185c2fe3911b3bca859
extracted tests.rs git blob: 3577e61a56cd
extracted tests.rs sha256: 8d4fc0013c6d99af8f9c19c8379bd15064c63307d0800f93fd3767118a4dc02b
extracted table.rs  sha256: e6a75bb0186188caecaad0ef9f3fdfd14dae439ac783e34e816e49237f674162
PRECONDITION lines in tests.rs: 1
MUTANT marker in table.rs (must be 0): 0
stamp comparison present in table.rs (must be >=1): 1
...
TARGET_EXIT=0 test_actually_ran=1 (want 1, want pass)
  test ... PRECONDITION stamp_before=(1791217927, 467100813, 1791217927, 467100813, 4096, 2) stamp_after=(1791131527, 0, 1791217927, 467100813, 4096, 2)
  test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 32 filtered out; finished in 0.00s
  no panic
  no assertion failure
```

`test_actually_ran=1` is derived from libtest's own `32 filtered out` accounting rather than from a
matching count, and the run aborts if the target test is missing from `--list`.
The run also asserts the mutant marker is absent from `table.rs` and the stamp comparison is present,
so it cannot have been served production source other than this branch's.
The remote-extracted `tests.rs` sha256 and git blob match the local branch exactly, so the host
measured the same bytes that were committed.

| check | host | exit | result |
| --- | --- | --- | --- |
| target test, `--exact`, one test | macOS | 0 | `1 passed; 32 filtered out` |
| target test, `--exact`, one test | Linux | 0 | `1 passed; 32 filtered out` |
| `cargo test --locked -p cowfs-vfs-path --lib`, all 33 | macOS | 0 | `33 passed; 0 failed; 0 ignored` |
| `cargo fmt --all -- --check` | macOS | 0 | clean |
| `cargo clippy --locked -p cowfs-vfs-path --all-targets -- -D warnings` | macOS | 0 | clean |
| `cargo test --locked -p cowfs-vfs-path --lib`, all 33 | Linux | 0 | `33 passed; 0 failed; 0 ignored` |
| `cargo fmt --all -- --check` | Linux | 0 | clean |
| `cargo clippy --locked -p cowfs-vfs-path --all-targets -- -D warnings` | Linux | 0 | clean |

## The earlier OLD/NEW/no-op-mutant matrix, carried explicitly

No fresh three-variant matrix was rerun, because the adaptation that would have invalidated it did not
happen.
Carrying it is bound to byte-identical responsible seams, checked by git blob:

| seam | this branch | saved tree that produced the matrix | old base `951045f` |
| --- | --- | --- | --- |
| `crates/cowfs-vfs-path/src/tests.rs` | `3577e61a56cd2f6f8f1decfe5ff1be0e4304110d` | `3577e61a56cd2f6f8f1decfe5ff1be0e4304110d` | `90de21aa99f22c1d75f6f45f571ff1d33087597b`, the OLD case |
| `crates/cowfs-vfs-path/src/table.rs` | `d61ca4226accb9db1bd15d739c0ccda43d8276d0` | `d61ca4226accb9db1bd15d739c0ccda43d8276d0` | `d61ca4226accb9db1bd15d739c0ccda43d8276d0` |
| `crates/cowfs-vfs-path/src/sys.rs` | `e94ebf03015fb557c4d953e4215382f092babcff` | `e94ebf03015fb557c4d953e4215382f092babcff` | `e94ebf03015fb557c4d953e4215382f092babcff` |

The shipped test file is the exact file the NEW and MUTANT cases ran, and the production seams are the
exact bytes the OLD case ran against.
Carried results: OLD exit 101, NEW exit 0 with a proven changed stamp, no-op cache invalidator MUTANT
exit 101, 33-test scoped suite, fmt, clippy.

The mutant patch `bench/out/pathvfs118/repair/pathvfs118-mutant.patch`, sha256
`aef69fa850f869ab026e010aa8f02e523722f1d14abcbe9cb86c40cbf8af00d3`, is read-only evidence and was not
shipped.
It touches `table.rs`, whose blob in this branch equals the old base's, so the no-op invalidator is
provably absent from what is being merged.

Both artifact hashes were re-verified at delivery time and match the recorded values.

## What the test now means, and what it does not

An external change whose effect is observable in the directory stamp invalidates the cached listing.
It no longer claims that an external change is visible immediately, because on these hosts it is not
and no test change can make it so.
The comment in the test says this and points at #120.

The expected-entries assertion is untouched: the resumed listing must produce `[a, b, c, d, e]`, and
the fresh-from-the-start listing must produce `[b, c, d, e]`, so the name created outside is still
required to be reachable through `PathVfs` and not merely present on the backing filesystem.
The precondition is asserted, not assumed: the test performs extra external mutations until the stamp
actually changes, then asserts it, rather than polling one unchanged event, and there is no arbitrary
sleep, no retry loop, no `#[ignore]`, no `cfg` gate, no skipped assertion and no threshold weakening.

#120, the same-tick coherence gap in the adapter itself, is explicitly not addressed here and remains
open.

## CI at the moment of writing

One snapshot, no polling and no rerun or dispatch.
`total_count=3` check runs: `check (macos-latest)` `in_progress`, `check (ubuntu-latest)` `in_progress`,
`linux-fuse` `in_progress`.
Commit status `pending`.
This is not a green claim and not a "no checks configured" claim.

## Review and merge

Independent review before merge.
This delivery did not merge.
No CHANGELOG or other auto-generated file was edited, and no co-author trailer was added.
