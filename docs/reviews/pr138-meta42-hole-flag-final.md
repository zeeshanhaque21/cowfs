# PR #138, issue #42 request 3: an explicit hole flag on `ChunkRef`

Independent critic review, scoped to request 3 only.
Verdict: **PASS, scoped.**
No behaviour defect found, no measured defect, nothing that blocks a merge decision by the coordinator.

## What was reviewed

| what | value |
|---|---|
| PR | https://github.com/zeeshanhaque21/cowfs/pull/138 |
| head | `99bf7a5efd28a80bc024f040efa2ae6fe0ca6c67` on `fix/meta-hole-flag-42` |
| base, and remote `main` at review time | `93cfef94457a989d031cb6b0a475ac4edbdb85ef` |
| commits | `25ceae4` feat, `37ab0bc` record the proof, `99bf7a5` correct the PR number in the record |
| lease | `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/3/cowfs`, held at `016769e7f4076a5c0fc712a65932c546048052f7`, clean, 0 porcelain lines |
| critic output | `bench/out/meta42-hole-flag-final-critic/`, new, this lane only |

Requests 1, 2, 4 and 5 of #42 are untouched by this PR and were not reviewed.

## Provenance correction, read before anything else

The digest handed to this review, `97564702ed9f08a27a2f9c2a6fed20f1fe34835bd3fb18de890d743a0efd216d`, is **not** the canonical record at the reviewed head.
It is the blob at parent commit `37ab0bc`.
The blob at head `99bf7a5` is `b0041044e58284eb2b3cd0dd06ff37368549369a88c0f64b5b7c9a2754a6bd97`, which differs because `99bf7a5` is titled "correct the PR number in the request-3 record".
The copy in the MAIN primary checkout is `b0041044…` and is byte-identical to the head blob, but untracked there (`??`).
All three revisions were found by walking every commit reachable from every ref.
Nothing else in the record moved.
The immutable residual `docs/verification/evidence/meta42-residual-verification.md` is `fbc6a078137b0fab370638d27dcaf64ff3ad283de37d8e7e970e4b10faac53ba`, unchanged, as claimed.
The eight sha256 prefixes the record publishes for the four files the change turns on all reproduce exactly, on the correct arm of the old/new pair.

## The ownership deviation, inspected rather than approved silently

The delivery touched exactly one line in `crates/cowfs-meta/src/tx.rs`, at `put_extent`:

```
-            encode_chunks(std::slice::from_ref(c)),
+            encode_chunks(std::slice::from_ref(c))?,
```

That is the necessary fallible-codec seam and nothing else.
`encode_chunks` now returns `Result` because `validate()` can refuse a ref, and `put_extent` has to carry that refusal.
The diff contains zero occurrences of `set_now` or `Timestamp::now`, so it does not touch #136's clock seam, which lives in a different function (`Tx::set_now`, added by #136 at `tx.rs:218-227`, against `put_extent` at `tx.rs:426-431`).
`crates/cowfs-meta/src/db.rs` is untouched, `--stat` empty.
So the deviation is one line, in the right function, mechanically required by the codec change, and disjoint from request 5.
Approved on that evidence.

## The contract, checked at the source

**The medium layout is unchanged.**
`encode_chunks` (`crates/cowfs-meta/src/types.rs:335-346`) writes `c.id.as_bytes()` then `c.len.to_le_bytes()` into a buffer of `chunks.len() * 36` bytes.
No flag byte, no high bit, no hash migration, no new format, no version marker.
A ref carries 36 bytes before the flag and 36 bytes after it.

**Decode is compatible with a store written before the flag existed.**
`decode_chunks` (`types.rs:348` onward) sets `hole` from `id == cowfs_store::HOLE`, so a legacy extent yields exactly the refs it always did, with the flag derived and costing nothing on the medium.
Extent overflow is already guarded: `b.len().is_multiple_of(36)` rejects a short or ragged segment before any split, and both halves are read through the bounds-checked `rd` helper, so a truncated tail is an error rather than a partial ref.

**An inconsistent ref is refused before anything reaches the medium.**
`encode_chunks` calls `c.validate()` before extending the buffer and maps a refusal to `Error::Invalid`.
`validate` (`crates/cowfs-store/src/lib.rs:133-143`) rejects three shapes: a flagged hole naming a stored block, a hole longer than `HOLE_MAX`, and the sentinel without the flag.

**A refusal is transactional.**
The new `?` in `put_extent` propagates out of `set_content` and `splice_content` (`tx.rs:444` and `tx.rs:482`).
`cowfs-core` reaches them only inside a batch (`crates/cowfs-core/src/inner.rs:938`, `sc.snap.batch(...)`).
`Snapshot::batch` (`db.rs:1724`) is `Meta::mutate`, which clones the tree before running the closure (`db.rs:731`, `let saved = e.tree.clone()`) and restores it on an ordinary error at `db.rs:749-753`, `e.tree = saved; self.note(&er); return Err(er)`.
So a mid-list refusal leaves no partial mutation, and the rollback is pre-existing machinery, not new code.

**One definition of the marker, re-exported rather than repeated.**
`HOLE` and `HOLE_MAX` now live in `cowfs-store` (`lib.rs:87-88`), `cowfs-core/src/file.rs:14` re-exports `HOLE`, and `cowfs-gc` re-exports it too.
`ChunkRef::hole_refs` is not decoration: it has four production call sites in `crates/cowfs-core/src/file.rs` at lines 227, 283, 301 and 318, the sparse-write and splice paths, so moving it beside the flag is justified and the thin wrapper at `file.rs:24` keeps those sites unchanged.

**The read-side guard moved, and it did not disappear.**
At base, `cowfs-core/src/file.rs` had `is_hole(c) = c.id == HOLE && u64::from(c.len) <= HOLE_MAX`, so a zero-id ref claiming more than a hole may claim was not a hole and surfaced as `Error::Corrupt` on read.
At head, `is_hole` is `c.is_hole()`, trusting the flag.
That guard now lives in decode-time `validate`, which rejects the same shape as `Error::Corrupt`, one step earlier.
The read path therefore keeps the same refusal, and a malformed legacy extent that used to be caught on read is now caught on decode.
The delivery's record states this move; it is correct and it is the only read-path semantic change.

**No new feature, no framework.**
Public surface added to `cowfs-store`: the `hole` field, `block`, `hole`, `is_hole`, `hole_refs`, `validate`, `HOLE`, `HOLE_MAX`, and the `ChunkRefError` enum.
Every one of those is either load-bearing above or examined below.
No module, no trait, no abstraction layer, no configuration, no new dependency.

## Runtime, in the mandated order

Two fresh archives bound to pinned commits, one per arm, each with its own `CARGO_TARGET_DIR` and `TMPDIR`.
The old arm was verified blob-for-blob against base, 641 tracked files, 0 mismatched.
The new arm was verified blob-for-blob against head, 645 tracked files, 0 mismatched.
The new arm then differs from the exact head only by my own fixture, and the old arm differs from the exact base only by the delivery's two fixtures and mine.

**Gate 0, before anything wider: one complete public-`Core` fixture.**
A sparse file of 4 MiB built only through the public `Vfs` surface, one 32-byte region written a megabyte in, `sync`, the metadata walk, then the same again through a fresh `Core::open` on the same directory.
Its first run failed one of four, and the failure was my assertion, not the code: I had asserted the refs sum to the file size, but a trailing hole is not a ref.
That is the delivery's own documented model, pinned by its test `the_trailing_hole_is_not_a_chunk_ref_and_is_not_walked`.
Corrected and re-run green.

| gate | what | old arm, base | new arm, head |
|---|---|---|---|
| R1 | delivery `hole_walk` | **0 passed, 3 failed, exit 101** | 3 passed, 0 failed, exit 0 |
| R2 | delivery `hole_flag` | does not compile, **29 errors**, exit 101 | 14 passed, 0 failed, exit 0 |
| R3 | my public-`Core` fixture, one byte-identical source | 1 passed, 3 failed, exit 101 | **4 passed, 0 failed, exit 0** |
| R4 | cross-binary legacy extent, both directions | 36 bytes, id 32 zero, len 1024 LE, identical both ways | |
| R5 | my layout probe, both arms | `refs=2 holes=1 covered=1048608 extent_bytes=72 shapes=hole(1048576)+block(32)` | byte-identical to the old arm |
| R6 | my bounds probe, new API so new arm only | not applicable | 1 passed |
| R7 | existing hole filter, GC mark, meta critic | not applicable | 2 passed, 11 passed, 12 passed |
| R8 | fmt and clippy | see below | exit 0 |

R1 is the mandated sentinel and it is exact: on the old arm the walk hands the caller `[BlockId(0000…0000), BlockId(94de2dd4…)]`, two names for one stored region, and the fixture's first sentinel assertion fails.
R3 reproduces that with an independent fixture on the same public surface, and the one test that passes on the old arm is the layout test, which is precisely the test that must agree across arms.

**R4 is the part the delivery's own record says it does not cover, and it is the strongest single piece of evidence here.**
It writes the chunk list with one binary and reads it with the other, on the same directory, in both directions.
Old writes, new reads: `extent_hex=000000000000000000000000000000000000000000000000000000000000000000040000`.
New writes, old reads: `extent_hex=000000000000000000000000000000000000000000000000000000000000000000040000`.
Identical, 36 bytes, id 32 zero bytes, length 1024 little-endian.
The old arm writes with a struct literal, the new arm writes with `ChunkRef::hole`, which also proves the new constructor encodes to exactly the legacy bytes rather than to a new shape.

**The stored bytes, not a fingerprint.**
The author's fixture checks that the walked block is present and readable.
Mine reads the block out of the store and compares it to the bytes that were written, byte for byte.
On the new arm the walk yields exactly one id, `store().get(id)` returns exactly `b"cowfs-independent-survivor-bytes"`, and `store().contains(sentinel)` is false, so the hole was never stored as a block.
After a reopen the same bytes come back, the walk is unchanged, the written region reads back exactly and the hole reads as 32 zero bytes.
So filtering the hole is proved not to have cost the data, not merely proved to have removed a name.

**R6, the boundary no delivery test crosses.**
`hole_refs` and `HOLE_MAX` appear in no file under any `tests/` directory in the whole tree, so the split of a run longer than one ref is untested by the delivery.
Measured independently: at exactly the bound, 1 ref; one byte over, 2 refs summing 1,073,741,825, which is the bound plus one, so the split neither loses nor invents a byte; 4 GiB, 4 refs summing 4,294,967,296.
A hole one byte over the bound, a hole naming a stored block, and the sentinel without the flag are all refused by `validate`.

**R8, static checks.**
`cargo clippy -p cowfs-store -p cowfs-meta -p cowfs-core -p cowfs-gc --all-targets -- -D warnings` exits 0.
`rustfmt --check` over the delivery's own eleven changed files exits 0.
`cargo fmt --all -- --check` over the new arm exits 0.
Every fmt failure observed anywhere in this lane was in a file I authored, and none was in the delivery.
An early `cargo fmt --all --check` exit 1 in the first pass was exactly that, my fixture, and it is disclosed here rather than dropped.

## Follow-ups, all documentation or simplification, none a behaviour bug

Each is a source check, not a measured defect.
The delivery's record acknowledges the first of these and defers it, because `crates/cowfs-core/src/lib.rs` sits next to clock work another branch owns.

**F1, stale doc.** `crates/cowfs-core/src/lib.rs:579-581`.
`Core::live_blocks` says "`cowfs-meta` yields the all-zero hole ref of a sparse file, which is not a block; this filters it", and "until `ChunkRef` has a hole flag".
After this change the walk does not yield the hole, so both sentences are now false.
Three lines to correct, in a file this PR does not touch.

**F2, now-unreachable arm.** `crates/cowfs-core/src/lib.rs:594`, `if id != file::HOLE` in `Core::live_blocks`.
Lines 588-593 feed it from `snap.live_blocks(&mut marker)`, which is the walk that now filters on the flag.
Unreachable, harmless.

**F3, a second unreachable arm the record does not list.** `crates/cowfs-core/src/lib.rs:463`, in `Core::fsck`.
Line 460 is `snap.live_blocks(&mut marker)`, so `if id == file::HOLE || store.contains(id)` has a first arm that can no longer be taken.
The record names four callers that compared the sentinel; this one is not among them.

**F4, the collector's arm is unreachable too.** `crates/cowfs-gc/src/lib.rs:709`, `if b == HOLE { continue; }`.
The walk it filters comes from `snap.live_blocks_with_root(marker)` at `gc:684`.
The record names the collector but attributes the redundancy only to `Core::live_blocks`.

**The distinction that stops F2 to F4 being over-applied, recorded so nobody removes the wrong line.**
`crates/cowfs-core/src/lib.rs:793`, in `Core::pinned_blocks`, is **not** redundant: lines 789-792 take ids from the cached in-memory `FileData.chunks.refs`, not from the walk, and those cached refs still carry holes.
`cowfs-gc/src/lib.rs:222`, `note_access`, is a public entry point called by a reader holding any block id and must stay defensive.
`gc:343` and `gc:743` are second filters over `pinned_blocks()` output, redundant in the harmless sense but defensible at a trust boundary.
Only the three walk-fed comparisons are genuinely dead.

**F5, granularity no caller can observe.** `crates/cowfs-store/src/lib.rs:151-163`.
`ChunkRefError` has three variants, constructed only inside `validate` at lines 135, 138 and 141, and named nowhere else in the tree, not in any test and not at any consumption point.
The single production consumer, `crates/cowfs-meta/src/types.rs:341`, collapses all three into `Error::Invalid("chunk ref is neither a hole nor a block")`.
So the enum plus its `Display` and `Error` impls carry a three-way distinction that no caller can tell apart.
Not a defect, and keeping a typed error at a crate boundary is defensible, but if the author wants one message this is where the twenty-odd lines go.

**F6, an untested production-reachable boundary.** `hole_refs`' split at `HOLE_MAX`, reached from four sites in `crates/cowfs-core/src/file.rs`.
No test in the tree crosses it.
R6 shows the behaviour is correct; a one-test addition to the delivery would remove the gap.

## PR identity, one read-only snapshot

| field | value |
|---|---|
| state | open, not draft |
| changed files | 22, +1,144 / -145 |
| `mergeable_state` | clean |
| `closing_issues_references` | null |
| issue #42 | open, body sha256 `67caa764c421b308…`, unchanged |
| closing forms in the three commits | none; `#42` appears only as a request reference |

Read-only merge-trees, no merge performed, no source written.
Against #136 head `e7ee215` → tree `610412f534f2d8f60b977d306a9222476a888bc2`, rc 0.
Against #137 `9d1e5ef` → tree `2a1b20b02e288a1069b7fe80f733b2581bdccfc8`, rc 0.
Against current remote `main` `93cfef9` → tree `0776c2dea07d792f2cbcd2150dc262097bf723eb`, rc 0.
No conflict in any direction, and the three branches touch disjoint lines.

CI at `99bf7a5`, read once, never polled, never re-run, no workflow or runner touched:
`linux-fuse` completed/success, `check (ubuntu-latest)` completed/success, `check (macos-latest)` completed/success.
Three of three, green at the reviewed head.

## What this review does not accept

The PR body claims no hole-family or fsx acceptance, no GC physical reclamation, no performance or timing acceptance, and no power-loss or crash claim, and says requests 1, 2, 4 and 5 are untouched and #42 stays open.
Nothing in this review accepts any of those.
Request 3 is the explicit flag and the safe-on-its-own walk, and only that was reviewed.
#42 remains open with requests 1, 3 and 4 still to do.

## Budget and lane discipline

| measure | value |
|---|---|
| bench/out before any build | 5,023,020 KiB |
| bench/out peak | 6,377,756 KiB against the 8,388,608 KiB cap |
| delta | 354,736 KiB, 0.34 GiB, against a projected 2.03 GiB |
| free floor at run | 297,440,424 KiB against the 20,480,000 KiB floor |
| heavy acquisitions | one 600 s foreground `mac-heavy.lock` per script, five scripts |
| logs | append and flush per gate, `bench/out/meta42-hole-flag-final-critic/logs/` |
| background commands | none, and nothing was polled |
| cleanup performed | none, no deletion, no move, no offload, no cap waiver |

The projection was made before building, from measured costs of my own earlier builds in this same lease, and it was conservative: two arms at 1,000,000 KiB each for a 2.03 GiB plan against 3.21 GiB of headroom.
The actual cost came in at 0.34 GiB, because the scope was the named tests rather than the whole suite.
The two-arm limit was the constraint that mattered: a third arm would not have fitted, so the plan was fixed at exactly the arms the work needs.

## Not run, and not claimed

`no-mistakes` is not initialized in this repo, `.no-mistakes` and `.claude` are both absent, so no no-mistakes gate ran.
`chromium` is not installed, so any browser step would be UNVERIFIED and none was attempted.
MisakaNet is local-only and was not consulted.
The author's 811-pass, 25-ignored full-suite figure was **not** independently re-executed.
The executed scope was the three walk tests, the fourteen flag tests, `caches`, `cowfs-gc --test mark`, `cowfs-meta --test critic`, `fmt` and `clippy`, plus my three probes.
`scripts/lane.sh` carries a header comment claiming a 60-second no-progress watchdog that the script does not implement; it checks the floor and the cap before acquisition and bounds acquisition at 600 seconds, and nothing more.
No browser claim, no timing claim, no interval assertion, no injected clock, no private probe in a production path.
Every value above came from an exact equality on public `Vfs`, `Meta`, `Snapshot`, `Store` or `ChunkRef` output, or from a byte comparison of decoded bytes.

## No action taken

Nothing outside `bench/out/meta42-hole-flag-final-critic/**` and this report was written.
No production edit, no checkout, no branch, no commit, no push, no merge, no lease action, no issue edit, no new issue, no signal to any process, no install, no sudo, no mount walk.
Prior reports and evidence left untouched: `c4999d45…`, `965d9c73…`, `de18fd78…`, `ed9609e2…`, `fbc6a078…`.
Raw logs from earlier lanes were not appended to; this lane's logs are new files under its own directory.

## Bottom line

The flag does what request 3 asked, the walk is safe on its own, the bytes on the medium are the bytes that were always written, and the refusal is transactional.
Every claim the delivery makes that is testable reproduced exactly: 29 compile errors on the base, 14 passing flag tests, three passing walk tests, fmt and clippy clean, eight source digests.
The claims it does not make were not tested and are not accepted here.
The six follow-ups are a stale doc, three dead comparisons, one over-specified error enum and one untested boundary, none of which changes behaviour.