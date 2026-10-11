# Bazel spike for the cowfs workspace on macOS (2026-10-10)

Question: can Bazel (rules_rust plus crate_universe) build cowfs on clones fast enough to meet "over 2x native cargo build time is not worth it", and does a shared disk cache cut disk across checkouts?
Status: done. Both phases ran.

## Verdict

- Feasibility: yes. The whole workspace (15 libraries, 3 bins, 168 test binaries, 10 examples) builds with 177 lines of BUILD Starlark and zero third-party overrides.
- Rule 1, edit rebuilds within 2x of cargo: FAIL. Leaf edit 5.1x (3.14 s vs 0.61 s, medians); store edit 2.6x (20.48 s vs 7.80 s).
- Rule 2, clean rebuild with warm disk cache within 2x: PASS. 0.09x in the same output base (1.66 s vs 18.31 s), 0.75x in a fresh clone at a new path (13.38 s vs 17.85 s).
- Rule 3, disk for 2 clones below two cargo target dirs: FAIL by about 13%. 8,657 MiB vs 7,652 MiB by df.
- Rule 4, under 200 BUILD lines: PASS at 177 lines; FAIL at 215 if `MODULE.bazel` and `.bazelrc` count.
- Bottom line: Bazel wins only on rebuilding something already in the cache.
  For the inner edit loop it is 2.6x to 5x slower than cargo, and its disk use is no better.
  By the user's 2x rule, Bazel is not worth adopting for cowfs on this Mac.

## Setup

- Machine: Apple M3 Max, 16 cores, 128 GiB RAM, macOS, APFS Data volume (same box as PR 337).
- Source: APFS clone (`cp -cR`) of the primary checkout at `12f94bee` into `.bench-bazel/c1` (only `crates/`, `Cargo.toml`, `Cargo.lock`, `.config`, `.gitignore`; no `target/`, no untracked agent dirs).
- Primary checkout untouched except one line `.bench-bazel/` in `.git/info/exclude`.
- Bazel 9.3.0 via bazelisk 1.29.0 (`.bazelversion`), rules_rust 0.74.0 from BCR (newest at run time), Bzlmod only.
- Rust toolchain: rules_rust downloads rustc 1.99.0 (same version as the host rustup toolchain used by cargo).
- crate_universe: `crate.from_cargo` over the root plus 15 member `Cargo.toml` files and the existing `Cargo.lock`, `supported_platform_triples = ["aarch64-apple-darwin"]`.
- All Bazel state kept under `.bench-bazel`: `--output_user_root`, `--repository_cache`, `--disk_cache`.
- Compilation mode `-c dbg` (debuginfo=2, opt-level=0) to match cargo's dev profile; Bazel's default `fastbuild` emits debuginfo=0 and would bias time and disk toward Bazel.
- Bazel target set (`//crates/...`, 196 targets): 15 `rust_library`, 3 bins, 18 unit-test harnesses (15 lib plus 3 bin), 150 integration tests, 10 examples.
- Cargo reference: `cargo test --no-run --workspace --offline`, the same command PR 337 used.

## Hand-written Starlark

| file | lines |
| --- | --- |
| `cowfs.bzl` (one macro: lib, bins, unit tests, integration tests, examples) | 61 |
| root `BUILD.bazel` | 1 |
| 15 per-crate `BUILD.bazel` (5 to 11 lines each) | 115 |
| BUILD subtotal | 177 |
| `MODULE.bazel` | 34 |
| `.bazelrc` | 4 |
| total | 215 |

No `crate.annotation`, no build-script overrides, no patches.
Third-party crates come from `all_crate_deps()`, so `Cargo.toml` stays the source of truth for external deps and features.

## Timeline and obstacles

1. First smoke target `//crates/cowfs-vfs` built first try: 75 s wall from nothing, most of it rules_rust bootstrapping its own tools (process_wrapper, cargo_toml_variable_extractor, cargo-bazel deps) and the crate_universe splice. Artifact validated as an `ar` rlib.
2. All 15 libraries plus 3 binaries built on the first attempt (35 s on top), including 19 third-party build scripts.
   Native C/asm build steps (zstd-sys C via `cc`, blake3 NEON/asm, crc32c) needed zero overrides.
   `cowfs --help`, `cowfs-treehouse --help` and `cowfs-daemon-bin --help` run from `bazel-bin`.
3. Adding tests and examples: three fixes, about 10 minutes total.
   - Daemon's `[[test]] path = "tests/guard/mod.rs"` root was also in the shared helper glob: duplicate label error. Fixed by filtering the root out of helpers.
   - `cowfs-meta/src/db.rs` pulls `#[path = "../tests/common/backend.rs"]` into its unit test: needed `compile_data` with `tests/**`.
   - `cowfs-store/tests/compact.rs` gates a helper on `#[cfg(feature = "fault-injection")]`: integration tests need the package's `crate_features`, which cargo passes implicitly.
4. Full `//crates/...` (196 targets) builds green.
   Run check: `bazel test` of `cowfs-snapname_unit` and `cowfs-vfs_unit` passed; no other tests were run.

Structural obstacles (not bugs, but real cost):

- Workspace path deps are duplicated by hand from `Cargo.toml` into BUILD files (`all_crate_deps` only covers registry crates); they drift unless a generator (for example gazelle_rust) is added.
- Feature unification: `cargo test --workspace` turns on `cowfs-store/fault-injection` and `cowfs-core/fault-injection` for the whole graph because dev-deps request it.
  Bazel has no per-invocation unification, so the spike enables `fault-injection` on the libraries unconditionally.
  A production build would need a `bool_flag` plus `select()`, or a second library variant; neither was built.
- `cowfs-daemon` depends on `cowfs-nfs` unconditionally (the mac branch of its `cfg(target_os)` deps); Linux needs a `select()` and `fuser` resolved for a Linux triple.
- Cargo-provided env vars need shims: `CARGO_BIN_EXE_cowfs`, `CARGO_BIN_EXE_cowfs-treehouse` (via `rustc_env` with `$(rootpath)`), `CARGO_TARGET_TMPDIR` (hardcoded `/tmp`).
  `CARGO_MANIFEST_DIR` is set by rules_rust, but test runtime data (`tests/battery/*`, `tests/golden/wire.tsv`, `../../bench/out`) is not declared as `data`, so tests that read it would fail at run time under Bazel's sandbox. Unverified.
- `cowfs-vfs-test/tests/mutations_full.rs` (needs `mutation-tests` feature) is excluded, as cargo excludes it by default.
- Bazel builds proc-macros and build scripts in the opt exec configuration; cargo builds them unoptimised in dev. Cargo also uses incremental compilation for workspace crates; Bazel does not. Both asymmetries are inherent to the default setups and are left in.

## Per-crate status

| crate | lib | bins | unit test | integration tests | examples | crate-specific lines |
| --- | --- | --- | --- | --- | --- | --- |
| cowfs-vfs | ok | | ok | 0 | | 1 |
| cowfs-snapname | ok | | ok | 0 | | 1 |
| cowfs-store | ok | | ok | ok | ok | 1 |
| nfsserve | ok | | ok | 0 | | 1 |
| cowfs-meta | ok | | ok | ok | ok | 1 |
| cowfs-gc | ok | | ok | ok | ok | 5 |
| cowfs-vfs-test | ok | | ok | ok (minus mutations_full) | | 5 |
| cowfs-vfs-path | ok | | ok | ok | | 1 |
| cowfs-fuse | ok (mac cfg) | | ok | ok | ok | 5 |
| cowfs-ctl | ok | | ok | ok | ok | 1 |
| cowfs-nfs | ok | | ok | ok | | 5 |
| cowfs-core | ok | | ok | ok | ok | 7 |
| cowfs-daemon | ok | ok | ok | ok | | 7 |
| cowfs-cli | ok | ok | ok | ok | | 7 |
| cowfs-treehouse | ok | ok | ok | ok | | 7 |

"ok" means compiles and links; only two unit-test targets were executed.
No crate needed more than about 5 minutes of fiddling; the 20-minute stop rule never triggered.

## Timing

Method:
- Started after `docs/reviews/clone-sccache-mac-20261010.md` appeared, at 18:08, when the 1-minute load average was 6.1.
- Driver: one Python script, strictly sequential, one CSV row per step (fsynced).
- Command: `cargo test --no-run --workspace --offline` vs `bazel build //crates/...` (`-c dbg`).
- Target parity: cargo reported 113 `Compiling` units and 168 test executables; Bazel has 168 `rust_test` targets.
- Edits as in PR 337: append `// bench ...` to `crates/cowfs-cli/src/lib.rs` (leaf) and to `crates/cowfs-store/src/lib.rs` (store).
- Cargo and Bazel interleaved per rep, with the order alternated, so both tools saw the same load drift.
- Single machine, other agents active: the load average rose from 6 to 82 during the run.
  Absolute seconds are noisy; the per-rep ratios are the signal.
- Disk: `sync; sleep 10; df -k /System/Volumes/Data` before and after the steps marked df.

| step | cargo s | Bazel s | ratio Bazel/cargo | load1 at step | reps |
| --- | --- | --- | --- | --- | --- |
| cold full build, nothing cached (Bazel: expunged output base, empty disk cache, repo cache warm, no network) | 16.49 | 73.46 | 4.5x | 5.6 / 14.9 | 1 |
| clean rebuild (cargo clean; Bazel `bazel clean`, warm disk cache) | 18.31, 19.83, 17.91 | 1.66, 1.72, 1.48 | 0.09x | 33 to 71 | 3 |
| Bazel `clean --expunge`, warm disk cache (server, externals, toolchain re-set up) | (18.31 median) | 15.19 | 0.83x | 70 | 1 |
| no-op | 0.11 | 0.41 | 3.7x | 53 | 1 |
| leaf edit (cowfs-cli lib) | 0.59, 0.61, 0.63 | 6.74, 3.14, 2.96 | 5.1x (medians 3.14 / 0.61) | 47 to 53 | 3 |
| store edit (cowfs-store lib) | 7.17, 7.80, 8.10 | 21.25, 18.80, 20.48 | 2.6x (medians 20.48 / 7.80) | 44 to 77 | 3 |
| clone 2 at new path, first build (Bazel: new output base, shared disk cache) | 17.85 | 13.38 | 0.75x | 82 / 48 | 1 |
| clone 2 no-op | | 0.40 | | 48 | 1 |
| clone 2 `bazel clean` then rebuild, warm cache | | 1.54 | | 48 | 1 |

Bazel action counts:
- Leaf edit: 6 sandboxed actions (lib, bin, 2 tests, unit harnesses).
- Store edit: 129 sandboxed actions, because every dependent rlib and every test binary that links store is rebuilt and relinked.
- Cargo reported `Compiling` for 7 packages.
- Clone 2 first build: all 533 compile actions were disk-cache hits, so the cache key does not depend on the workspace path.
- Disk-cache hits cost about 1.5 s when the output base is warm.
  With a fresh output base they cost 13 to 15 s, almost all of it server start, toolchain and external repo setup, and the crate_universe splice.

Disk (MiB, df is the primary figure, du is listed for reference):

| item | df delta | du |
| --- | --- | --- |
| cargo c1 cold build (`target/`) | 3,835 | 3,979 (4,449 at end, after edits) |
| cargo c2 build (`target/`) | 3,817 | 3,984 |
| cargo, 2 clones | 7,652 | |
| Bazel c1 cold build (output base plus disk cache) | 8,030 | 8,002 output base + 7,368 disk cache |
| Bazel c2 first build (new output base, shared cache) | 627 | 2,365 output base |
| Bazel, 2 clones | 8,657 | 6,649 + 2,365 output bases + 7,654 disk cache at end |
| Bazel repository cache (toolchain and crate downloads; the cargo analogue is `~/.cargo/registry`, excluded on both sides) | not measured | 1,344 |

Block sharing:
- Disk-cache blobs and output-base files share blocks, but not through hardlinks.
  `stat` shows link count 1 on the store rlib in both output bases, and `find disk-cache -links +1` finds 0 of 3,734 files.
- The df delta is far below the du sum: 8.0 GiB vs 15.4 GiB on the cold build, and 0.6 GiB vs 2.4 GiB for clone 2.
  Only APFS block cloning explains that.
  This is an inference from df vs du with link count 1; I did not trace whether the clonefile call comes from Bazel or the JDK.
- Clone 2 therefore costs only about 0.6 GiB by df.
- The first output base plus the disk cache cost 8.0 GiB, about twice one cargo `target/`.
  The fixed cost (cache and first output base) is only recovered from about the 3rd clone on.
  This is an estimate from the measured 8,030 + 627 per extra clone vs 3,826 per cargo clone: 8,030 + 627(n-1) < 3,826n gives n >= 3.
- PR 337 arm B (an APFS clone of an already-built cargo base) costs less per extra clone without Bazel at all.

## Rule evaluation

1. Rule 1 (Bazel edit rebuilds within 2x of native cargo): FAIL.
   - Leaf edit 5.1x (3.14 s vs 0.61 s).
   - Store edit 2.6x (20.48 s vs 7.80 s).
   - Every rep failed: per-rep ratios were leaf 11.4x, 5.1x, 4.7x and store 3.0x, 2.4x, 2.5x.
   - The PR 337 cargo reference (leaf about 0.8 s, store about 8 s) agrees with this run's cargo numbers.
2. Rule 2 (clean rebuild with warm disk cache within 2x of cargo clean build): PASS.
   - 0.09x in the same output base.
   - 0.75x to 0.83x with a fresh output base (new clone, or after `--expunge`).
3. Rule 3 (Bazel total disk across 2 clones below two cargo target dirs): FAIL.
   - 8,657 MiB vs 7,652 MiB by df (+13%).
   - It would pass from 3 clones on (estimate above).
4. Rule 4 (whole workspace with under 200 hand-written BUILD lines): PASS on the plain reading, BUILD files plus the macro at 177 lines.
   - With `MODULE.bazel` and `.bazelrc` it is 215 lines: FAIL by 15.

## Anomalies

- Load average was 33 to 82 for every step after the cold one (other agents).
  The Bazel cold build (load 14.9) ran under more load than the cargo cold build (5.6), which biases the 4.5x ratio slightly against Bazel.
- Bazel's first leaf-edit rep (6.74 s) is an outlier vs 3.14 s and 2.96 s; the median is used.
- `du` of Bazel trees over-counts APFS clones; this is why df is the primary disk figure, as in PR 337.
- At the end, the c1 output base by du (6,649) is smaller than right after the cold build (8,002).
  `bazel clean --expunge` followed by a disk-cache refill was not attributed further.
- The Bazel-downloaded rustc 1.99.0 is the same version as the host toolchain, but not the same binary.

## Not verified

- Tests were compiled, not run, apart from 2 unit-test targets that passed.
  Test runtime data (`tests/battery/*`, golden files, `CARGO_TARGET_TMPDIR=/tmp`) is undeclared and likely breaks some tests under the sandbox.
- Linux: not attempted (`cowfs-fuse`/`fuser` and the daemon's `cfg(target_os)` deps would need `select()`).
- Bazel speed-ups not tried, which could move rule 1:
  - `--spawn_strategy=local` (no darwin-sandbox per action).
  - rules_rust experimental incremental compilation.
  - Pipelined compilation.
  - Splitting test binaries.
  The 2.6x to 5x gap is measured only for the default setup.
- A production (non-test) build without forced `fault-injection` was not built.
- A disk-cache trim or GC policy was not exercised; the cache only grows (7.4 to 7.7 GiB over this run).
- Single host, one run per cold step, under heavy shared load.

## Cleanup

- `bazel shutdown` was run in c1 and c2.
- Deleted: `.bench-bazel/` (clones, output user root, disk cache, repo cache).
- Also deleted: `~/Library/Caches/bazel` and `~/Library/Caches/bazelisk`, both created by this spike's first `bazelisk version` at 17:41 (birth time checked).
- The `.bench-bazel/` line stays in `.git/info/exclude`.

## Reproduce

The Starlark (MODULE.bazel, `cowfs.bzl` macro, per-crate BUILD calls) and the driver `bench.py` are not committed.
To rebuild them, see the "Hand-written Starlark" and "Timeline and obstacles" sections above.
The key choices were `crate.from_cargo` with all 16 manifests, `all_crate_deps()` for external deps, and hand-listed workspace path deps.

## Appendix: spike Starlark (verbatim)

`MODULE.bazel`:

```starlark
module(name = "cowfs")

bazel_dep(name = "rules_rust", version = "0.74.0")

rust = use_extension("@rules_rust//rust:extensions.bzl", "rust")
rust.toolchain(edition = "2021", versions = ["1.99.0"])
use_repo(rust, "rust_toolchains")
register_toolchains("@rust_toolchains//:all")

crate = use_extension("@rules_rust//crate_universe:extensions.bzl", "crate")
crate.from_cargo(
    name = "crates",
    cargo_lockfile = "//:Cargo.lock",
    manifests = [
        "//:Cargo.toml",
        "//crates/cowfs-cli:Cargo.toml",
        "//crates/cowfs-core:Cargo.toml",
        "//crates/cowfs-ctl:Cargo.toml",
        "//crates/cowfs-daemon:Cargo.toml",
        "//crates/cowfs-fuse:Cargo.toml",
        "//crates/cowfs-gc:Cargo.toml",
        "//crates/cowfs-meta:Cargo.toml",
        "//crates/cowfs-nfs:Cargo.toml",
        "//crates/cowfs-snapname:Cargo.toml",
        "//crates/cowfs-store:Cargo.toml",
        "//crates/cowfs-treehouse:Cargo.toml",
        "//crates/cowfs-vfs:Cargo.toml",
        "//crates/cowfs-vfs-path:Cargo.toml",
        "//crates/cowfs-vfs-test:Cargo.toml",
        "//crates/nfsserve:Cargo.toml",
    ],
    supported_platform_triples = ["aarch64-apple-darwin"],
)
use_repo(crate, "crates")
```

`.bazelrc`:

```starlark
startup --output_user_root=/Users/zeeshanhaque/Projects/cowfs/.bench-bazel/out-user-root
common --repository_cache=/Users/zeeshanhaque/Projects/cowfs/.bench-bazel/repo-cache
build --disk_cache=/Users/zeeshanhaque/Projects/cowfs/.bench-bazel/disk-cache
build -c dbg
```

`cowfs.bzl`:

```starlark
load("@crates//:defs.bzl", "all_crate_deps")
load("@rules_rust//rust:defs.bzl", "rust_binary", "rust_library", "rust_test")

LINTS = ["-Dunsafe_code", "-Wmissing_debug_implementations"]

# One macro per crate: lib, bins, unit test, integration tests, examples.
# Mirrors `cargo test --no-run --workspace` (workspace-unified features).
def cowfs_crate(name, deps = [], dev_deps = [], features = [], bins = {}, tests = None, env = {}, data = []):
    ext = all_crate_deps(normal = True)
    ext_dev = all_crate_deps(normal_dev = True)
    pm = all_crate_deps(proc_macro = True)
    pm_dev = all_crate_deps(proc_macro_dev = True)
    lib = ":" + name
    rust_library(
        name = name,
        srcs = native.glob(["src/**/*.rs"], exclude = ["src/main.rs"]),
        crate_root = "src/lib.rs",
        crate_name = name.replace("-", "_"),
        crate_features = features,
        deps = deps + ext,
        proc_macro_deps = pm,
        rustc_flags = LINTS,
        visibility = ["//visibility:public"],
    )
    for bin, src in bins.items():
        rust_binary(name = bin, srcs = [src], deps = [lib] + deps + ext, proc_macro_deps = pm, rustc_flags = LINTS)
        rust_test(name = bin + "_unit", crate = ":" + bin, deps = dev_deps + ext_dev, proc_macro_deps = pm_dev, rustc_flags = LINTS)
    rust_test(
        name = name + "_unit",
        crate = lib,
        compile_data = native.glob(["tests/**/*.rs"], allow_empty = True),
        deps = dev_deps + ext_dev,
        proc_macro_deps = pm_dev,
        rustc_flags = LINTS,
    )
    tdeps = [lib] + deps + dev_deps + ext + ext_dev
    tpm = pm + pm_dev
    helpers = native.glob(["tests/*/**/*.rs"], allow_empty = True)
    roots = tests if tests != None else native.glob(["tests/*.rs"], allow_empty = True)
    for root in roots:
        rust_test(
            name = root.replace("tests/", "it_").replace("/", "_").removesuffix(".rs"),
            srcs = [root] + [h for h in helpers if h != root],
            crate_root = root,
            deps = tdeps,
            proc_macro_deps = tpm,
            crate_features = features,
            rustc_env = env,
            data = data,
            rustc_flags = LINTS,
        )
    for ex in native.glob(["examples/*.rs"], allow_empty = True):
        rust_binary(
            name = ex.replace("examples/", "ex_").removesuffix(".rs"),
            srcs = [ex] + native.glob(["examples/*/**/*.rs"], allow_empty = True),
            crate_root = ex,
            crate_features = features,
            deps = tdeps,
            proc_macro_deps = tpm,
            rustc_flags = LINTS,
        )
```

`crates/cowfs-daemon/BUILD.bazel`:

```starlark
load("//:cowfs.bzl", "cowfs_crate")

exports_files(["Cargo.toml"])

cowfs_crate(
    "cowfs-daemon",
    bins = {"cowfs-daemon-bin": "src/main.rs"},
    deps = ["//crates/" + c for c in ["cowfs-ctl", "cowfs-core", "cowfs-gc", "cowfs-store", "cowfs-vfs", "cowfs-vfs-path", "cowfs-vfs-test", "cowfs-nfs"]],
    dev_deps = ["//crates/cowfs-meta"],
    tests = glob(["tests/*.rs"]) + ["tests/guard/mod.rs", "tests/evidence/mod.rs"],
)
```

`crates/cowfs-cli/BUILD.bazel`:

```starlark
load("//:cowfs.bzl", "cowfs_crate")

exports_files(["Cargo.toml"])

cowfs_crate(
    "cowfs-cli",
    bins = {"cowfs": "src/main.rs"},
    data = [":cowfs"],
    deps = ["//crates/cowfs-ctl", "//crates/cowfs-daemon"],
    env = {"CARGO_BIN_EXE_cowfs": "$(rootpath :cowfs)"},
)
```

Other per-crate BUILD files have the same shape: one `cowfs_crate(...)` call with workspace `deps`/`dev_deps` copied from Cargo.toml.

## Appendix: raw results.csv

```csv
step,tool,clone,secs,load1,df_used_mib,note
cold_full,cargo,c1,16.49,5.62,3834.6,compiling=113 executables=168
cold_full,bazel,c1,73.46,14.9,8029.9,"1574 processes: 1041 internal, 533 darwin-sandbox."
du_after_cold,cargo,c1,,,3979.1,du target
du_after_cold,bazel,c1,,,8001.5,du output_base
du_after_cold,bazel,,,,7368.3,du disk_cache
clean_rebuild_0,cargo,c1,18.31,32.81,,compiling=113 executables=168
clean_rebuild_0,bazel,c1,1.66,56.0,,"1574 processes: 533 disk cache hit, 1041 internal."
clean_rebuild_1,bazel,c1,1.72,54.96,,"1574 processes: 533 disk cache hit, 1041 internal."
clean_rebuild_1,cargo,c1,19.83,54.96,,compiling=113 executables=168
clean_rebuild_2,cargo,c1,17.91,70.55,,compiling=113 executables=168
clean_rebuild_2,bazel,c1,1.48,69.86,,"1574 processes: 533 disk cache hit, 1041 internal."
expunged_warm_disk_cache,bazel,c1,15.19,69.86,,"1574 processes: 533 disk cache hit, 1041 internal."
noop,cargo,c1,0.11,53.04,,compiling=0 executables=168
noop,bazel,c1,0.41,53.04,,
leaf_edit_0,cargo,c1,0.59,53.04,,compiling=1 executables=168
leaf_edit_0,bazel,c1,6.74,53.04,,"7 processes: 5 action cache hit, 1 internal, 6 darwin-sandbox."
leaf_edit_1,bazel,c1,3.14,50.07,,"7 processes: 5 action cache hit, 1 internal, 6 darwin-sandbox."
leaf_edit_1,cargo,c1,0.61,46.78,,compiling=1 executables=168
leaf_edit_2,cargo,c1,0.63,46.78,,compiling=1 executables=168
leaf_edit_2,bazel,c1,2.96,46.78,,"7 processes: 5 action cache hit, 1 internal, 6 darwin-sandbox."
store_edit_0,cargo,c1,7.17,43.6,,compiling=7 executables=168
store_edit_0,bazel,c1,21.25,46.11,,"134 processes: 119 action cache hit, 5 internal, 129 darwin-sandbox."
store_edit_1,bazel,c1,18.8,67.15,,"134 processes: 119 action cache hit, 5 internal, 129 darwin-sandbox."
store_edit_1,cargo,c1,7.8,76.38,,compiling=7 executables=168
store_edit_2,cargo,c1,8.1,76.07,,compiling=7 executables=168
store_edit_2,bazel,c1,20.48,77.03,,"134 processes: 119 action cache hit, 5 internal, 129 darwin-sandbox."
c2_first_build_shared_cache,bazel,c2,13.38,82.2,627.4,"1574 processes: 533 disk cache hit, 1041 internal."
c2_noop,bazel,c2,0.4,48.26,,
c2_clean_warm_cache,bazel,c2,1.54,48.26,,"1574 processes: 533 disk cache hit, 1041 internal."
c2_first_build,cargo,c2,17.85,48.26,3817.1,compiling=113 executables=168
du_final,bazel,c1,,,6649.0,du output_base
du_final,bazel,c2,,,2365.4,du output_base
du_final,bazel,,,,7653.8,du disk_cache
du_final,bazel,,,,1344.0,du repo_cache
du_final,bazel,,,,9120.2,du out_user_root (all output bases + install)
du_final,cargo,c1,,,4449.1,du target
du_final,cargo,c2,,,3984.3,du target
```
