# Spike 1: dedup ratio on the real worktree corpus

Issue: #1.
Tool: `spikes/dedup-corpus`.
Run date: 2026-09-29, against live treehouse pools (read-only).

## Result

The design gate is met.
The dedup ratio was measured on the real corpus before any filesystem code was written.

| Method | Size | Ratio vs raw |
|---|---|---|
| Do nothing (each inode counted once) | 273.43 GiB | 1.00x |
| Per-file zstd-3 only | 76.01 GiB | 3.60x |
| Whole-file dedup only | 87.61 GiB | 3.12x |
| FastCDC dedup only (16/64/256 KiB) | 53.80 GiB | 5.08x |
| FastCDC dedup then zstd-3 (the cowfs design) | 19.48 GiB | 14.04x |

Over per-file compression alone, the cowfs design is 3.9x smaller (76.01 vs 19.48 GiB).
FastCDC beats whole-file dedup by 1.6x on raw bytes (87.61 vs 53.80 GiB), which supports the choice of content-defined chunking over whole-file hashing.

## Marginal cost of one more slot

This is the number that matters for treehouse.
In the lumen pool (29 slots, processed in numeric order), the first slot costs 2.27 GiB compressed.
Each later slot costs a median of 0.47 GiB compressed, which is 4.9% of its 10.07 GiB raw size.
The maximum was 1.70 GiB and the minimum was 0.00 GiB.
Attribution is order-dependent.
Totals are not.

## Read this before quoting the headline

The 14x figure is a Rust build-output result from one repository.
- 266.49 of 273.43 GiB raw (97.5%) is under `target/`.
- 250.86 GiB raw (91.7%) is the single lumen pool.
- Sharing across different pools is small: summing per-pool unique bytes and subtracting global unique bytes saves only 10.00 GiB raw.
- Only 5.38 GiB of unique data is shared by two or more pools.
- Outside `target/`, source and `node_modules` are 6.91 GiB raw and dedup only to about 3.1 GiB, so most of their gain comes from compression.

A different workload mix will not reproduce 14x.

## Sample and exclusions

- 45 slots: 44 treehouse slot directories across 22 pool directories, plus one Node project (`context-mode`).
- 843,889 regular files and 273.43 GiB read.
- 3,851,941 chunks, 807,246 unique, average unique chunk 71,555 bytes.
- 333,194 hardlinked paths skipped because their inode was already counted.
  On lumen slots 1 to 3 all such links were in cargo `target/` and none of the 10,472 linked inodes was shared across slots.
  That check was not repeated for the other slots.
- 57 symlinks skipped and not followed.
- Loose treehouse state files in pool roots were excluded.
- 0 read errors, 0 walk errors, 0 files changed mid-read.
- The lumen pool has 29 slot directories today and all 29 were walked.
  An earlier listing suggested about 33.
  Slots are created and returned while agents work, so the set was not stable.

## Validation

On a 3-slot sample (mlx-serve, OmniRoute, context-mode) an independent Python implementation (`spikes/dedup-corpus/validate.py`) matched the tool exactly on file count (73,678), raw bytes (2.80 GiB) and whole-file unique bytes (2.59 GiB).
The chunking and compression figures were not independently re-derived.
The full run was not independently re-derived either.

## Not measured and unverified

- APFS clones are invisible to a userspace scan.
  Real physical disk use of the corpus may be lower than the raw figure.
  It was not measured, so the baseline is logical bytes with hardlinks counted once.
- Metadata is not included: the Merkle tree, redb entries and pack index for 807,246 chunks and 843,889 files.
- One chunking configuration and one compression level were tried.
  There was no parameter sweep.
- Keys are 128-bit truncated BLAKE3.
  This is fine for measurement and not for the store.
- Compression is per chunk with no dictionary.
- The corpus is a single point in time.
- Reconciling `du` with the tool's raw size for the lumen pool was not done.
  `du` reported about 280 GiB at 10:30 and the tool read 250.86 GiB at about 11:00.
  The slot set and block rounding both differ, and neither was checked.
