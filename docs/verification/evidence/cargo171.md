# Cargo byte-identity at a canonical path (issue 171)

Date: 2026-10-08.
Question: does the byte-identical-artifact promise of issue 17 cover `cargo build`, and what is the smallest setting that makes it true?

## Fixture and method

A three-crate workspace in `cargo171/fixture`: a proc-macro crate `mac`, a library `lib` that uses it, and a binary `app` with a `build.rs`.
No registry dependencies.
`cargo171/run.sh LABEL N ENV=VAL` deletes `target`, runs `cargo build --workspace` at one fixed directory, and records sha256 of every file under `target` (except `.cargo-lock`), N times.
`cargo171/an.py` reports, per path, whether all N builds agree.
Layout the scripts expect: the fixture copied to `proj/` next to `run.sh`.
Toolchain: rustc 1.99.0 and cargo 1.99.0 on both hosts.

Hosts:

- Linux: the cachyos box (kernel 7.2.8, ext4, unprivileged user namespaces allowed), workspace `/mnt/docs/Projects/cowfs-cargo171`.
- macOS: this Mac (Darwin 25.6, arm64).

## 1. Same-path rebuild, default dev profile (before)

Linux, N = 5, default settings (incremental on for workspace members):

- 438 file instances over 5 builds, 41 identical (9.4%), because the incremental directory is named with a random per-session id (`debug/incremental/<crate>-<h>/s-<session>-<h>/...`), so those paths never even repeat.
- Ignoring the incremental directory, 43 files exist in every build and 41 are identical.
- The two that differ: `debug/deps/liblib-<h>.rlib` and its hard link `debug/liblib.rlib`.
- Why, checked with `ar t` on two clean rebuilds on Linux: the archive member name differs, `lib-<h>.<cgu>.10knou8.rcgu.o` against `lib-<h>.<cgu>.1g8ytb9.rcgu.o`.
  The trailing token is the per-session id that incremental compilation puts in codegen-unit object names, so the rlib bytes change with it.
- The binary `app`, the proc-macro `.so`, the build-script outputs, the dep-info `.d` files and the fingerprint files were identical in all 5 builds on Linux for this fixture.

macOS, N = 5, default settings:

- Beyond the incremental directory, the binary `debug/app`, `liblib.rlib`, `libmac.dylib`, and about 140 per-codegen-unit `.o` files under `debug/deps` differ in every build.
- Cause on macOS: debug builds link with the object files' paths and modification times in the debug map, and the object files are rewritten each build.
  This is stated as the likely mechanism, not isolated further, because macOS is not a canonical-namespace host.

## 2. Same-path rebuild with `CARGO_INCREMENTAL=0` (after)

- Linux, N = 5: 43 of 43 paths identical in all 5 builds, including `app`, both rlibs, the `.so`, `.d` and fingerprint files.
- macOS, N = 5: 46 of 46 paths identical in all 5 builds, including `debug/app` and the dylib.

## 3. Cross-slot at the canonical path, Linux (the real promise)

`cargo171/ns.sh`: two copies of the fixture (`slotA`, `slotB`) built through `scripts/cowfs-ns-run.sh` at one canonical directory; A1 and A2 are `slotA` twice, B1 is `slotB`, N1 is `slotA` built at its own path (the native control).

| Setting | A1 vs A2 | A1 vs B1 | A1 vs N1 (own path) |
|---|---|---|---|
| `CARGO_INCREMENTAL=0` | 0 differing | 0 differing | 15 differing |
| `CARGO_INCREMENTAL=1` | 81 differing | 81 differing | 94 differing |

With incremental on, 79 of the differences are in the random-named incremental directory and the other 2 are `liblib-<h>.rlib` and `liblib.rlib`.
With incremental off, nothing differs between slots at the canonical path, and the control N1 differs in 15 paths, among them `debug/app`, `deps/liblib-<h>.rlib`, `deps/libmac-<h>.so` and their `.d` files, which is the embedded absolute path.
So both ingredients are needed: the canonical path removes the path difference (A1 vs N1), and `CARGO_INCREMENTAL=0` removes the run-specific bytes (A1 vs A2).

## Relation to the earlier 92% figure

`docs/linux-namespaces.md` used to say, from spike 6, that same-path debug rebuilds are 92% identical and blamed the incremental query cache and the proc-macro dylib.
Neither number nor the dylib part reproduced on Linux here: the proc-macro `.so` was identical in 5 of 5 default builds, and only the macOS dylib differed.
The incremental cache part did reproduce.
The 92% came from a different fixture and metric (spike 6), and the 41 of 43 above is a file count on this fixture, so the two are not comparable.

## What was not tried

Clean rebuilds only: every build started from an empty `target`, and `target` sat inside the snapshot.
A `CARGO_TARGET_DIR` outside the snapshot, and incremental rebuilds over an existing `target`, were not measured.
`--remap-path-prefix`, `SOURCE_DATE_EPOCH` and `-C strip` were not needed on Linux, so they were not adopted; they are untested here.
Release profile, registry dependencies, `cargo test` and a cargo workspace with `-C debuginfo` changes were not measured.
One fixture, one toolchain version, one kernel.
