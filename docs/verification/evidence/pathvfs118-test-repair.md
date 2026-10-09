# Test repair for issue #118: make the external-change precondition observable

Refs #118, which tracks the `cowfs-vfs-path` readdir failure, and #19 behind it.
The diagnosis this rests on is `docs/verification/evidence/pathvfs118-diagnosis.md`, sha256
`fc73a654c8ee12b73d28cd4e55eec5c425f6e27e063dc7137cb7689bfe4a55c1`, accepted.

**This is a test-only change and it is not applied anywhere.** PR #113 is under independent review at
`b434afe98f95db3ad5c134e80747ccad0e1a5732`, so nothing was written to its branch, its head, or the
primary checkout's source. The change exists as a patch artifact for the coordinator to deliver.

| | |
| --- | --- |
| patch artifact | `bench/out/pathvfs118/repair/pathvfs118-test-repair.patch` |
| patch sha256 | `efcd6f4e7e83ec0d255700d39907e8c2004cc3ef0d25c43d314b9b2dc2e766b5` |
| mutant patch | `bench/out/pathvfs118/repair/pathvfs118-mutant.patch`, sha256 `aef69fa850f869ab026e010aa8f02e523722f1d14abcbe9cb86c40cbf8af00d3` |
| exact source base | `origin/main` = `951045fca4823611e196eda75db0c977a46d2c77`, 555 files |
| base archive | `base-main-951045f.tar.gz`, sha256 `5b7be45a0e21927a4ba47db6ee5c952f10e56eac84700c9b96d0309eeb1de89b` |
| patched tree | `src.tar.gz`, sha256 `0dfd724409f5f17ad390547a4de027ff2f3146a33f3aa9dfc13b05a8392a1d88` |
| pristine tree | `src-old.tar.gz`, sha256 `9eebc15634428419be2c22da310a34e38008cdbaf6abcccd888a9590cea0de9f` |
| mutant tree | `src-mutant.tar.gz`, sha256 `ebfc6dd78212f29d92b016b29d9367cb9aa21cb4c67d57c438b281685a8f63e9` |
| lock edge | `Cargo.lock` sha256 `2ed20c88136771f956e4170aa248e5261b3249a2ba4a0e4cb6b6d49aa8e46f77`, unchanged in all three trees, and unchanged by every cargo command |
| files changed | `crates/cowfs-vfs-path/src/tests.rs` only, 77-line patch. The mutant additionally touches `table.rs` and is labelled as a mutant |

## What the diagnosis changed about the proposal

The proposal in the diagnosis, polling `fstat` after the external mutation until the stamp moved, was
**wrong**, and the correction is worth stating. Polling cannot help: if the host never performs another
write, no amount of reading makes the stamp advance. The directory is only timestamped when something
changes it. A poll would wait for the full deadline and then fail, on every host, for a reason that has
nothing to do with the cache.

What the test needs is not a wait but a precondition it establishes itself: after the external create
and removal, write a known distinct directory timestamp with the standard library, then prove the
six-field stamp actually differs before resuming the listing. `std::fs::File::set_times` with
`FileTimes::set_modified` is stable since Rust 1.75, works on both host families here, and is an
explicit `futimens`, so it takes effect at once however coarse the host's directory clock is. The
value chosen is one day before the current mtime, which no tick boundary can put back.

## The change

Three parts, all in `crates/cowfs-vfs-path/src/tests.rs`:

1. `dir_stamp`, which reads the same six fields `table.rs:176-184` builds its cache key from, so the
   test checks the exact thing the Vfs compares.
2. `force_observable_mtime`, which sets the directory's mtime a day into the past.
3. In the test itself: read the stamp the Vfs has just recorded, make the external change, force the
   stamp, read it again, and `assert_ne!` the two. If a host cannot make the stamp observable at all,
   that assertion fails loudly. Nothing is skipped and no assertion is weakened.

The expected-entries assertion is untouched: the resumed listing must still produce
`[a, b, c, d, e]`, and the fresh-from-the-start listing must still produce `[b, c, d, e]`. Only its
failure message grew a clause saying the stamp changed, because that is now part of what the test
establishes.

**What the test now means, precisely.** An external change whose effect is observable in the directory
stamp invalidates the cached listing. It no longer claims that an external change is visible
immediately, because on this host it is not, and no test change can make it so. The comment in the test
says this and points at #120.

## Results on the host that reproduces the failure

Host: `moonscapenas`, Linux `6.12.109+rpt-rpi-2712` aarch64, rustc 1.95.0.
Remote root: `/home/moonscape/cowfs-ready-wave/task-19/`. One `CARGO_TARGET_DIR` per tree, because
sharing one across the three trees made cargo reuse a stale artifact and produced a void result that
is described in §5.

All three cases run the same test, `--exact tests::readdir_sees_a_name_created_outside_the_vfs_while_a_listing_is_paged`,
one test ran in each, derived from libtest's own `32 filtered out` accounting rather than from a
matching count.

| case | tree | exit | outcome |
| --- | --- | --- | --- |
| OLD | pristine `951045f` | **101** | panicked at `tests.rs:328:5`, `left: [[97], [98], [99], [100]]` |
| NEW | patched `tests.rs` only | **0** | `1 passed`, full expected entries, precondition proven |
| MUTANT | patched `tests.rs` + no-op cache invalidation | **101** | panicked at `tests.rs:377:5`, `left: [[97], [98], [99], [100]]` |

### Direct stamp before and after, NEW

```
PRECONDITION stamp_before=(1791172597, 21523963, 1791172597, 21523963, 120, 2)
PRECONDITION stamp_after =(1791086197, 0,        1791172597, 21523963, 120, 2)
```

The mtime moved back by exactly 86400 seconds, `1791172597 - 86400 = 1791086197`, and the nanosecond
field went to zero, which is what an explicit `futimens` produces. `ctime` did not move, because the
clock that stamps `ctime` is the same coarse one; the mtime field carries the change, which is why
setting it explicitly is what makes the precondition establishable.

Note the `size` field: `120` before and `120` after. A create and a removal net to the same entry
count, so `size` never distinguished this case on either host. Only a timestamp can, which is the
substance of #120.

### The mutant, and what the native filesystem sees

The mutant removes the stamp comparison from `readdir_page`'s restart condition, leaving
`cookie == 0 || listing.is_none()`. The first page still works, because nothing is cached yet, and
every later page reuses the cache forever.

Its result is the point of the whole exercise. The mutant fails with `e` missing even though its
precondition line proves the stamp did change:

```
PRECONDITION stamp_before=(1791172613, 761407685, 1791172613, 761407685, 120, 2)
PRECONDITION stamp_after =(1791086213, 0,        1791172613, 761407685, 120, 2)
panicked at crates/cowfs-vfs-path/src/tests.rs:377:5
  left: [[97], [98], [99], [100]]
 right: [[97], [98], [99], [100], [101]]
```

So the strengthened precondition does not make the test lenient. With an observable stamp and a cache
that ignores stamps, the name created outside is still not seen. The test discriminates the cache, not
the clock.

The native-versus-`PathVfs` distinction is therefore unchanged and not hidden by any of this. On this
host, in the unpatched run, the file `e` exists on the backing filesystem, its creation by
`std::fs::write` succeeded, and the directory's stamp did not move, so `PathVfs` resumed from its
cached listing and omitted `e`. In the patched run the fresh-from-the-start listing assertion, which
runs after the resumed one, passes with `e` present, so the name is reachable through `PathVfs` the
moment the cache is not in the way. That gap, a create and a removal inside one directory-clock tick
being invisible to a resumed listing, is the adapter behaviour issue #120 tracks. This repair does not
fix it, does not claim to, and does not hide it: the precondition is asserted, not assumed, and the
comment in the test names #120.

## Scoped checks, true exit codes

Run in the patched tree on the same host, in the same locked invocation:

| check | exit | result |
| --- | --- | --- |
| `cargo test -p cowfs-vfs-path --lib`, all 33 | **0** | `33 passed; 0 failed; 0 ignored` |
| `cargo fmt --all -- --check` | **0** | clean |
| `cargo clippy -p cowfs-vfs-path --all-targets -- -D warnings` | **0** | clean |

No full-workspace build, no stress suite, no corpus copy. Nothing here reruns the 500-sample probe or
the 20-run repetition from the diagnosis.

## Two mistakes of mine, recorded rather than dropped

**A shared target directory voided the first repair run.** All three trees pointed at one
`CARGO_TARGET_DIR`, cargo reused a stale artifact, `Finished in 0.04s`, and the suite step ran a binary
that did not contain the patch while reporting a test result that looked real. The `--exact` filter
also matched nothing in that run, which is a second way the same run could have read as a pass. Both
are fixed: one target directory per tree, and every case now derives its "did the test run" from
libtest's `32 filtered out` accounting and aborts if the target test is absent from `--list`.

**A type error in my own helper.** `dir_stamp` first returned `(i64, i64, i64, i64, i64, i64)`, but
`MetadataExt::size` and `::nlink` return `u64`. Clippy caught it as `E0308` at `tests.rs:310` and
`:311`. The shipped patch returns `(i64, i64, i64, i64, u64, u64)`, matching `table.rs`'s own tuple.

Both are why the first repair run's "suite still failing" reading was wrong rather than a finding
about the patch.

## Scope

One test file. No production source. `table.rs` is untouched in the shipped patch; the only `table.rs`
change anywhere here is inside the labelled mutant. No sleep, no `#[ignore]`, no `cfg` gate, no skipped
assertion, no macOS-only path, and no reliance on a green Ubuntu run to excuse the host failure.

The original logs, the invalid no-op probe correction, and the still-unverified `CONFIG_HZ` and
`ubuntu-latest` granularity from the diagnosis are unchanged and still open. Nothing in this repair
resolves them.

## Delivery

The coordinator applies `pathvfs118-test-repair.patch` to whichever branch should carry it. It was not
applied to the primary checkout's source, to the PR #113 branch, or to any commit, because PR #113 is
under review and this task has no authorization to change a branch. Issue #120 stays open for the
adapter behaviour itself.