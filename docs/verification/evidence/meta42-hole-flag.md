# Issue #42 request 3: an explicit hole flag, and a metadata walk that is safe on its own

Scope: request 3 of issue #42, "A hole flag in `ChunkRef`".
Requests 1, 2, 4 and 5 are untouched.

| what | value |
|---|---|
| branch | `fix/meta-hole-flag-42`, cut with `git switch -c` from the verified main commit |
| main commit it is based on | `93cfef94457a989d031cb6b0a475ac4edbdb85ef` |
| implementation head | `25ceae472cb81370cc5daf579395e7e8ba2a8c98` |
| PR | https://github.com/zeeshanhaque21/cowfs/pull/138 |
| lease | `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/6/cowfs` |
| accepted source audit, immutable | `docs/verification/evidence/meta42-residual-verification.md`, sha256 `fbc6a078137b0fab370638d27dcaf64ff3ad283de37d8e7e970e4b10faac53ba` |
| prior request-5 branch preserved | `fix/deferred-operation-time-42`, PR #136, untouched at `e7ee215878cc0102ce52c7611dddc81f064105f6` |
| toolchain | `rustc 1.99.0 (b940084d7 2026-09-28)`, macOS |

## The contract, from the issue and from the tree

Issue #42 request 3:

> A hole is currently an all-zero block id, which is why GC (#10) must use `Core::live_blocks` (it filters hole refs) rather than the walker in meta. A `ChunkRef` flag would make the walker itself safe.

`docs/v1-core.md:273-275` already reserves the sentinel and says a hole is never stored:

> `ChunkRef.id` all-zero can never be a real BLAKE3 output in practice, and #10 has to skip it when marking.

So the sentinel stays. What changes is that the type says which refs are holes instead of every reader of a chunk list having to know the rule.

What the accepted audit measured at this base commit:

- `ChunkRef` was `{ id, len }`, 6 lines, no flags.
- `cowfs-meta/src/walk.rs:101-102` queued `c.id` for **every** ref, with no filter, so `Snapshot::live_blocks` (`db.rs:1685`) yielded the sentinel.
- `cowfs-core/src/lib.rs:577-581` said it in the source: "`cowfs-meta` yields the all-zero hole ref of a sparse file, which is not a block; this filters it, so every id here is in the store. GC (#10) should use this, not the walker directly, until `ChunkRef` has a hole flag."
- Four callers each compared the sentinel themselves: `Core::live_blocks` (`lib.rs:594`), `Core::fsck` (`lib.rs:463`), `Core::pinned_blocks` (`lib.rs:793`), the collector (`cowfs-gc/src/lib.rs:709`).
- Two `HOLE` definitions: `cowfs-core/src/file.rs:14` and `cowfs-gc/src/lib.rs:44`.

## What was built

`ChunkRef::hole: bool` in `crates/cowfs-store/src/lib.rs`, with `ChunkRef::block(id, len)` and
`ChunkRef::hole(len)` constructors, `ChunkRef::is_hole()`, `ChunkRef::hole_refs(len)`, and one
`HOLE` and `HOLE_MAX` definition beside the flag.
`cowfs-gc` re-exports `cowfs_store::HOLE` rather than defining its own, so there is one definition.

The flag is **decided where the ref is decoded**, `decode_chunks` at `crates/cowfs-meta/src/types.rs:344`:
a zero id becomes a hole, a real id does not.
`LiveBlocks` at `crates/cowfs-meta/src/walk.rs:102` then filters on the flag rather than yielding every id.
That one line is the change that makes the walk safe on its own.

### The encoding on the medium does not change

An extent is still 36 bytes, id then length, and a hole is still 32 zero bytes followed by its
length.
A store written before the flag existed decodes to the same refs and re-encodes to the same bytes.
Nothing is migrated, no store is touched, no length high bit is added, no BLAKE3 id is
reinterpreted and there is no format version.

Only the Merkle root and the inode record can differ between two stores holding equivalent chunk
lists, because those carry fields this change does not touch.
No claim is made that a whole database is byte-identical.

### Invalid flag and id combinations are refused before they reach the medium

`ChunkRef::validate` rejects three shapes:

| shape | what it would do if encoded |
|---|---|
| a hole that names a stored block | the walk would skip it and the named block would never be marked live |
| a stored block with the sentinel id | a collector would be handed the sentinel as a block |
| a hole longer than `HOLE_MAX` | the ref would claim more zeros than a hole may, and a reader would treat it as a real block |

It runs at the existing fallible seam for a chunk list: `encode_chunks` is now `Result`, and
`Tx::put_extent` at `crates/cowfs-meta/src/tx.rs:426` propagates with `?`.
`set_content` and `splice_content` therefore refuse the whole batch with `Error::Invalid` and write
nothing.
On the read side `decode_chunks` reports the same shape as `Error::Corrupt`, which is what a
hand-edited store produces.

The one line touched in `tx.rs` is `put_extent`, which is not the timestamp seam: PR #136 added
`Tx::set_now` at `tx.rs:218-227`, and `put_extent` at `tx.rs:426-431` is a different function.
No other clock line is touched, and `db.rs` is untouched.

Two facts worth stating rather than discovering later:

- `validate` is **not** a `const fn`, because `BlockId`'s `PartialEq` is derived and `==` is not const.
- The reason is not carried across the crate boundary, because `cowfs_meta::Error::Invalid` holds `&'static str`.

## The literal-construction census, done before the first edit

Every `ChunkRef { .. }` literal and every `HOLE` definition in the tree at the base commit, and
where this change touches each:

| site | what it is | action |
|---|---|---|
| `cowfs-store/src/lib.rs:68` | the struct itself | flag, constructors, `validate`, `HOLE`, `HOLE_MAX` |
| `cowfs-store/src/store.rs:1389`, `:1419` | `ingest_bytes`, `ingest` | through `ChunkRef::block` |
| `cowfs-core/src/file.rs:31`, `:269` | `hole_refs`, `put_piece` | `hole_refs` delegates to the store; `put_piece` uses `block` |
| `cowfs-meta/src/types.rs:352` | `decode_chunks` | sets the flag from the sentinel |
| `cowfs-meta/src/check.rs:638` | a test that builds an extent to tamper with | `block` |
| `cowfs-meta/tests/{posix,review,model,critic,common}` | test helpers | `block` |
| `cowfs-meta/examples/{bench,review_bench}` | benchmark helpers | `block` |
| `cowfs-gc/tests/mark.rs:59` | a sparse fixture | `cowfs_store::ChunkRef::hole` |
| `cowfs-core/tests/critic2b.rs:511`, `:554` | two forged refs | `hole`, and the over-long one carries the flag explicitly |
| `cowfs-gc/src/lib.rs:44` | the `HOLE` definition | re-exports the store's |
| `cowfs-core/src/file.rs:14` | the other `HOLE` definition | re-exports the store's |

Nothing outside this list needed changing, and no caller outside it was found.

## Proof

### Source binding

Both old runs came from a `git archive` of `93cfef9` with only the fixture added and nothing else,
in its own target dir, under one bounded 600 s acquisition of the shared lane per run.

| archive | commit | tracked | mismatched | differs from base outside the fixture |
|---|---|---|---|---|
| `old-src` | `93cfef9` | 641 | **0** | **0** |
| `new-src` | `93cfef9` plus this change | 641 | **0** | 18 modified, 1 added, exactly the files in the census |

sha256 of the files the result rests on:

| file | old-src | new-src |
|---|---|---|
| `crates/cowfs-store/src/lib.rs` | `1110e7ff400ee793…` | `4613a829bc5bd384…` |
| `crates/cowfs-meta/src/types.rs` | `154bc7951a41eb63…` | `dd6ba118f89255a5…` |
| `crates/cowfs-meta/src/walk.rs` | `8bff4277599f2bc6…` | `3c6c57b70c5375c9…` |
| `crates/cowfs-meta/src/tx.rs` | `5cafb0cc2e10f1e2…` | `27faf6dc14808912…` |
| `crates/cowfs-core/src/file.rs` | `35e21a7424956f7b…` | `0595479135361d7c…` |
| `crates/cowfs-core/tests/hole_walk.rs` | `88fc09585a1a01ec…` | `88fc09585a1a01ec…` |

### Old fail, new pass: the walk stops handing out holes

`crates/cowfs-core/tests/hole_walk.rs` uses no `ChunkRef` literal and no flag, so it compiles and
runs against the tree before the flag existed as well as against the tree with it.
It makes the sparse file through the public `Core` API and calls the metadata walk itself, which is
what a collector would see.

| build | `hole_walk` | exit |
|---|---|---|
| `93cfef9` plus the fixture only | **0 passed, 3 failed** | 101 |
| this head | **3 passed, 0 failed** | 0 |

The old run's message, with the sentinel first in the list:

```
the metadata walk handed a hole to the caller as if it were a block:
[BlockId(0000000000000000000000000000000000000000000000000000000000000000),
 BlockId(7bf9a6c04a32a2008dbd62ef625d68469aa261b2e73b5238cc29d5ca9694590a),
 BlockId(99e926e1e8989d50c0c99739fc9d59ce5fe6f454d46d6f93e5905a774dc5b7cd),
 BlockId(a85a969c7174728fdfba2dacb1112bdb64884344c115dac64f96166a1d985502)]
```

The three tests and what each pins:

| test | pins |
|---|---|
| `the_metadata_walk_of_a_sparse_file_yields_only_stored_blocks` | a sparse file and a dense file in one snapshot: the walk yields only stored blocks, never the sentinel, and every id it names is in the store |
| `the_metadata_walk_and_the_stored_blocks_agree` | the survivor readback: the walked block is in the store and readable, the bytes the mount returns across the hole are the bytes written |
| `a_walk_after_a_reopen_still_yields_no_hole` | the same after a close and a fresh `Core::open` on the directory |

### Old compile failure: the metadata fixture

`crates/cowfs-meta/tests/hole_flag.rs` uses the field, the constructors and `validate`, none of
which exist at the base commit.

| build | `hole_flag` | exit |
|---|---|---|
| `93cfef9` plus the fixture only | does not compile: `no field named hole`, `no associated function block`, `no associated function hole`, `no method named validate`, `cannot find value HOLE in crate cowfs_store`, 29 errors | 101 |
| this head | **14 passed, 0 failed** | 0 |

Reported as what it is: a compile failure, not a passing test.
The runtime evidence is `hole_walk`, which runs on both.

### What the metadata fixture covers

| test | pins |
|---|---|
| `the_walk_yields_the_real_blocks_and_not_a_hole` | one block, an interior hole, another block: the walk yields exactly the two block ids |
| `the_flags_survive_a_close_and_a_reopen` | the flags decode as holes and blocks on both sides of the round trip, and the walk agrees after a reopen |
| `a_file_of_only_holes_yields_nothing` | a file with no stored block yields no block |
| `a_file_with_no_chunks_at_all_still_yields_nothing` | an empty file yields nothing |
| `the_encoding_of_a_hole_is_unchanged_by_the_flag` | one extent is 36 bytes, a hole's id is 32 zero bytes, the length follows: the legacy layout |
| `the_trailing_hole_is_not_a_chunk_ref_and_is_not_walked` | bytes past the chunk total stay a trailing hole with no ref |
| `a_marker_reused_across_a_walk_still_skips_a_shared_subtree` | the incremental-mark skip is unchanged by the filter |
| `a_walk_that_fails_still_fails_and_yields_no_partial_claim` | a walk over a snapshot with no file yields nothing |
| `a_ref_that_claims_a_hole_and_an_id_is_refused` | flag, id and length must agree |
| `a_ref_with_no_flag_and_the_zero_id_is_refused` | the mirror case |
| `a_hole_longer_than_a_hole_may_claim_is_refused` | `HOLE_MAX` is the bound and the bound itself is legal |
| `a_well_formed_ref_validates` | both legal shapes pass |
| `the_metadata_store_refuses_a_chunk_list_it_cannot_represent` | the batch refuses and writes nothing |
| `a_zero_id_ref_longer_than_a_hole_may_claim_is_corrupt_on_read` | the same refusal seen from the write side |

### Two existing tests changed, and why

`crates/cowfs-core/tests/caches.rs::live_blocks_filters_holes_and_yields_only_stored_blocks`
asserted that the metadata walk **still** yields the hole, with the comment "meta's own walker
still yields the hole, which is why the filter lives here".
That is the defect this request is about, so the test now asserts that the two walks agree exactly
and that neither hands out the sentinel.
The name is unchanged and the property it was named for still holds.

`crates/cowfs-core/tests/critic2b.rs::the_zero_id_is_never_a_real_block_and_a_hole_is_length_bounded`
forged a zero-id ref longer than `HOLE_MAX` through a batch and read it back expecting
`Error::Corrupt`.
The batch now refuses that ref, so the test asserts the refusal and that the batch left nothing
behind.
The store never held a hole, and BLAKE3 is still asserted never to produce the sentinel, which is
the first half of that test and is unchanged.

### Test matrix, every exit code read directly

| command | result | exit |
|---|---|---|
| `cargo test -p cowfs-meta -p cowfs-store -p cowfs-core -p cowfs-gc -p cowfs-daemon` | 91 suites, **811 passed, 0 failed**, 25 ignored | 0 |
| `cargo test -p cowfs-core --test hole_walk`, `93cfef9` + fixture | **0 passed, 3 failed** | 101 |
| `cargo test -p cowfs-core --test hole_walk`, this head | 3 passed, 0 failed | 0 |
| `cargo test -p cowfs-meta --test hole_flag`, this head | 14 passed, 0 failed | 0 |
| `cargo test -p cowfs-meta --test hole_flag`, `93cfef9` + fixture | does not compile | 101 |
| `cargo fmt --all -- --check` | clean | 0 |
| `cargo clippy -p cowfs-store -p cowfs-meta -p cowfs-core -p cowfs-gc --all-targets -- -D warnings` | clean | 0 |

The 25 ignored are reported as ignored, not as passed.
They are pre-existing `#[ignore]` weights, not tests skipped for this change.

## What this deliberately does not claim

- **No hole-family or fsx acceptance.** Three fixtures and the five crates' suites, nothing wider.
- **No performance claim.** No measurement was taken, and other workers are active.
- **No power-loss claim.** No `SIGKILL` was sent; the crash suites passed, which is evidence nothing
  regressed, not evidence of power-loss behaviour.
- **`Core::live_blocks` still filters.** It is now redundant for holes, since the walk does not yield
  them, but it is the collector's entry point and removing it is not this request. The `until` clause
  in its doc comment at `lib.rs:581` is now out of date and is left for the coordinator, because that
  file is next to the clock work another branch owns.
- **`CHUNK` extents written by an older store decode correctly** because the flag is derived, but no
  test writes bytes with an old binary and reads them with this one; the equivalence is argued from
  the unchanged 36-byte layout and pinned by `the_encoding_of_a_hole_is_unchanged_by_the_flag`.
- **Whole-database identity is not claimed.** The chunk extents are byte-identical; a Merkle root or an
  inode record may differ for other reasons.
- **`cargo test --workspace` was not run.** The five affected crates and CI cover it.
- **CI on this head is one snapshot**, reported when read, not polled.

## Untouched, deliberately

Requests 1, 2, 4 and 5 of #42: no snapshot rename, no shared name crate, no reservation API, no
clock work.
`crates/cowfs-core/src/inner.rs` untouched.
The timestamp seam in `cowfs-meta`'s `tx.rs` and `db.rs` untouched apart from the single `?` in
`put_extent`.
`crates/cowfs-daemon`, `PathVfs`, swap, rename, the inode policy and the health counters untouched.
The docs lock-audit row added by #136 untouched.
No new dependency, no format version, no migration, no store modified.

## Evidence

Under `bench/out/meta42-hole-flag/` in the lease, gitignored, with a separate `CARGO_TARGET_DIR` per
source tree:

| file | what |
|---|---|
| `OLD-hole-walk.log` | `hole_walk` on `93cfef9`: 3 failures with the sentinel in the list |
| `NEW-fixtures.log` | both fixtures on this head: 3 passed and 14 passed |
| `OLD-fixture.log` | `hole_flag` on `93cfef9`: the compile errors |
| `all-suites.log` | the five crates' suites, 91 suites |
| `meta-store-suite.log` | `cowfs-meta` and `cowfs-store` suites |
| `core-gc-daemon-suite.log` | `cowfs-core`, `cowfs-gc`, `cowfs-daemon` suites |
| `fmt.log`, `clippy.log` | the two static checks |
| `old-src/`, `new-src/` | the extracted trees, with the source binding above |
| `old-target/`, `new-target/` | the isolated target dirs |
