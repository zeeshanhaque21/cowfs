# Spike 6: artifact byte-identity across slots at different paths

Issue: #6.
Scripts: `spikes/nfs-loopback/spike6/`.
Raw data: `spikes/nfs-loopback/out/spike6/results.json` (git-ignored, 6 MB).
Run date: 2026-09-29, cargo 1.98.1, macOS 26, native APFS, `cargo build --frozen`.

## Result

Slots that share a warm base and then diverge by one edit stay cheap to store, with or without path remapping.
Slots built independently at different paths are not: a default debug build costs 12.8% to 28% of the first slot, and remapping paths or turning off incremental compilation brings that under 3%.
Per-agent mount namespaces (#17) are not needed for dedup on these numbers.
All numbers come from two small crates and one run per cell.

## Setup

- Crate X is a copy of `spikes/dedup-corpus` (34 build units).
- Crate Y is a copy of `spikes/nfs-loopback` (41 build units, tokio and vendored `nfsserve`).
- Each variant builds slots 1 and 2 independently, each at its own absolute path in its own pool directory, mimicking `{pool}/{slot}/{repo}`.
- "Identical" means byte-equal by relative path, with hardlinks counted per path and random incremental and codegen-unit names normalised.
- Slot 2 marginal cost is the spike 1 tool's new compressed bytes for slot 2 divided by slot 1 (FastCDC 16/64/256 KiB then zstd-3).
- I recomputed every marginal and identical percentage in the tables below from the raw JSON and they match.

## Independent builds at two different paths

Values are crate X / crate Y.
The last column is a same-path rebuild control (bytes identical after `rm -rf target` and rebuilding in the same directory).

| Variant | Identical files % | Identical bytes % | Slot 2 marginal % | Same-path rebuild bytes identical % |
|---|---|---|---|---|
| debug default | 52 / 34 | 66 / 56 | 12.8 / 28.0 | 92 / 88 |
| release default | 85 / 83 | 77 / 82 | 1.8 / 1.8 | 100 / 100 |
| debug, remap slot prefix | 76 / 84 | 85 / 73 | 2.3 / 5.9 | 92 / 88 |
| release, remap | 59 / 55 | 96 / 87 | 0.6 / 1.0 | 100 / 100 |
| debug, `debug=0` | 88 / 90 | 75 / 66 | 10.7 / 24.9 | 98 / 91 |
| debug, line tables only | 52 / 34 | 67 / 59 | 11.8 / 26.1 | 92 / 86 |
| debug, split-debuginfo off | 63 / 44 | 66 / 55 | 13.9 / 29.1 | 91 / 85 |
| debug, split-debuginfo packed | 62 / 44 | 61 / 49 | 12.7 / 26.8 | 92 / 86 |
| debug, remap and `debug=0` | 70 / 75 | 89 / 70 | 2.1 / 4.9 | 98 / 91 |
| debug, `CARGO_INCREMENTAL=0` | 84 / 81 | 73 / 75 | 2.9 / 3.7 | 100 / 100 |
| debug, remap and `CARGO_INCREMENTAL=0` | 63 / 63 | 90 / 86 | 0.55 / 0.74 | 100 / 100 |

Also run: `-Ztrim-paths` via `RUSTC_BOOTSTRAP=1`, which is a nightly hack.
Stable cargo rejects it ("feature trim-paths is required").
It matched remap: 2.1% / 5.5% for debug and 0.5% / 0.8% for release.

### What is and is not identical by default

- Identical: `build-out`, `.fingerprint`, and most build-script files.
- Mostly identical: `.o`, and most `rlib` and `rmeta`.
- Same content after mapping the slot path: `.d` dep-info and incremental `.o` files.
- Never identical: the final binary (about 1.4 to 2.5 KB differ), proc-macro dylibs (about 48 bytes, cause not confirmed, possibly code-signature or UUID bytes), and incremental `query-cache`.
- Embedded slot paths are the only cause of slot-to-slot differences.
  The `rlib` and `rmeta` differences cascade from `serde_core`'s build-script `OUT_DIR` path, which changes its crate hash, and that hash is embedded in each dependent (about 50 to 100 bytes per file, so chunking keeps most chunks shared).
- Release rebuilds at the same path are 100% byte-identical.
  Debug rebuilds are not: the binary, the zstd-sys build-script output, and incremental `query-cache.bin` change between builds.

## Warm base copied to a new path (the treehouse mode (b) case)

- **Fresh:** a warm `target/` copied with `cp -c -R -p` to a new path built as 34 of 34 (X) and 41 of 41 (Y) Fresh, with 0 crates compiled, for debug, release, `debug=0` and split-debuginfo off (n=1 each).
- **Remap breaks it:** with a slot-specific `--remap-path-prefix` the build compiled 34 of 34 and 41 of 41 units, because `RUSTFLAGS` is part of the fingerprint.
  Copying and keeping the old flags stays Fresh but the copied paths are not remapped.
- **After a one-line edit:** only one unit recompiles.

### Dedup cost after seeding by clone, then the same edit in both slots

Cost of the edited clone relative to slot 1, same units as above.

| Variant | X | Y |
|---|---|---|
| Clone, before any edit | 0% | 0% |
| Clone, debug default, after edit | 10.3% | 8.8% |
| Independent second slot, debug default, after edit | 16.7% | 31.9% |
| Clone, `CARGO_INCREMENTAL=0`, after edit | 1.3% | not run |
| Clone, remap with old flags kept, after edit | 4.2% | 6.5% |
| Clone, then remap to the clone's own prefix | 0.58% | 0.88% |

- The clone-seeded cost comes only from the workspace crate's binary and incremental directory.
- Remap with a clone that keeps the old flags is worse than independent remapped slots: the clone's real paths are embedded as differently-sized strings, which shifts debug info offsets.
- Cloning and then remapping to the clone's own prefix recompiles every unit, so it gives up the warm base.
- The clone-seeded 8.8% to 10.3% is the closest analogue of spike 1's median extra lumen slot cost of 4.9%.

## Answers to the issue

1. Cargo output is mostly path-independent for external dependencies, and path-dependent for the workspace crate's own binary, incremental data and the crate hashes derived from build-script output directories.
2. Remapping the slot prefix or turning off incremental compilation raises identity most.
   Both together give the best debug result (0.55% and 0.74% marginal).
   `debug=0` and split-debuginfo settings do little.
3. Remap flags that differ per slot make a copied `target/` fully dirty.
   Without them a copied `target/` at a new path stays Fresh.
4. On these crates, per-agent mount namespaces (#17) are not needed for dedup, and macOS remap flags are not needed for the warm-base design.
   They matter for mode (a), where unmodified treehouse builds independently at different paths and pays 12.8% to 28% per debug slot without them.

## Limits and caveats

- One run per cell.
  Determinism is inferred from the same-path controls only.
- Two small crates.
  A large workspace crate has a much larger unshared share (binary, incremental data), so 5% to 30% here does not extrapolate.
  Real lumen slots were not built.
- The workspace crate's binary and incremental data dominate the residual, which is why the clone-seeded cost sits near 10% for debug.
- Machine load average exceeded 30 on 15 of 26 variant runs, up to 75.
  Bytes are not timing sensitive, so no timings are reported.
- The CDC and zstd numbers were not independently re-derived.
  The whole-file check matched the tool on X-debug (1,344 files, 268,200,368 raw bytes, 156,174,368 whole-file unique).
- Crate Y's copy also contained about 860 KB of other agents' spike files, too small to matter.
- The pools and copies (about 17 GiB) were deleted after recording.
