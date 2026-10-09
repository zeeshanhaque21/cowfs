# Namespaces 171 rows 3 and 4: a real treehouse lease, and btrfs / XFS / ext4

Date: 2026-10-08 (box clock 17:15 to 17:25).
Issue: 171, rows 3 (real treehouse lease run on Linux) and 4 (filesystem matrix).
Scripts used, copied from the box: `docs/verification/evidence/namespaces171/` (`matrix171.sh`, `lease171.sh`, `fsimg.sh`, `onfs.sh`).
Raw logs stayed on the box under `/mnt/docs/Projects/cowfs-171/logs/` and were left in place.

## Host and tree

- Host: the cachyos desktop, `zeeshan@100.122.64.51`, kernel 7.2.8-2-cachyos, x86_64, 16 cores.
- Load before starting: `uptime` showed 0.02 0.14 0.16, three logged-in sessions, no other cowfs, cargo or rustc process.
  Load after: 0.83.
- Toolchain: rustc 1.99.0 (b940084d7 2026-09-28), cargo 1.99.0 (5f94df478 2026-08-27), Python 3.14.7, fusermount3 3.18.3.
- Everything lived under `/mnt/docs/Projects/cowfs-171`.
  `CARGO_HOME`, `CARGO_TARGET_DIR` (for the product build only), `TMPDIR` and `XDG_CACHE_HOME` were under it.
  `/` and `/home` were not written.
  `RUSTUP_HOME=/home/zeeshan/.rustup` was read only, to find the toolchain.
- Tree shipped: `git merge-tree --write-tree origin/main c1071a2`, which is `origin/main` at 55b69bc (includes PR 172, the CI isolation job, and PR 163) merged with the head of PR 180 (`c1071a2`, `CARGO_INCREMENTAL=0` on the canonical route).
  PR 180 merged afterwards as 43fe43c.
  `git diff ceb21a71c338e794bd9a25d094282d77cbf31789 43fe43c -- crates scripts bench` is empty, so the code measured equals `crates`, `scripts` and `bench` on main at 43fe43c.
  Tree id `ceb21a71c338e794bd9a25d094282d77cbf31789`, shipped with `git archive <tree> | ssh ... tar -x`.
- `cargo build -j 4 -p cowfs-cli -p cowfs-treehouse -p cowfs-daemon` succeeded on the box.

## Smoke first (btrfs, `/mnt/docs`)

Before any matrix, one complete result on the existing btrfs `/mnt/docs`:

- `python3 -m unittest discover -s bench -p test_namespaces.py -v` with `TMPDIR` on btrfs: `cowfs-ns-run: a mount namespace was available, so the isolation matrix ran`, `Ran 17 tests`, `OK (skipped=1)`.
- `matrix171.sh <dir> 6 0`: 6 canonical cargo builds plus 1 native build, byte diff of every file under `target`.
  Result below, validated against the numbers in `cargo171.md` (43 files, 15 files differing from the native control, 81 differing with incremental on).
  They matched, so the harness was sound before it was repeated.

## Row 4: filesystem matrix

Filesystems:

- btrfs: `/mnt/docs` (`/dev/nvme0n1p3`, `rw,noatime,compress=zstd:1,ssd,discard=async,space_cache=v2`).
  The loop images below also sit on this btrfs.
- ext4: 4 GiB sparse image `img/ext4.img` under the workspace, `losetup --find --show`, `mkfs.ext4 -q -F`, mounted `rw,relatime`.
- XFS: 4 GiB sparse image `img/xfs.img`, `mkfs.xfs -q -f`, mounted `rw,relatime,inode64,logbufs=8,logbsize=32k,noquota`.

Method, per filesystem: `TMPDIR=<fs>/tmp` so the test `src` and `canonical` directories are on that filesystem, then `onfs.sh <label> <fs dir>`, which runs the isolation tests and then `matrix171.sh <fs dir>/run 6 <I>` for `I` in 0 and 1.
`matrix171.sh` copies the `cargo171` fixture (proc-macro crate, library, binary with `build.rs`, no registry dependencies) into `slotA` and `slotB`.
It builds 6 times through `scripts/cowfs-ns-run.sh --src <slot> --canonical <canon> -- env CARGO_INCREMENTAL=<I> cargo build --workspace`, alternating slotA, slotB, slotA, slotB, slotA, slotB, each from an empty `target`.
It then builds once at `slotA`'s own path with no helper (the native control, the do-nothing baseline).
Each build is a sha256 manifest of every file under `target` except `.cargo-lock`.
`target` sits inside the slot.

Isolation tests (`bench/test_namespaces.py`), by name, on each filesystem:

| Filesystem | Ran | Result | 9 Isolation tests | 7 Refusals tests | Skipped |
|---|---|---|---|---|---|
| btrfs | 17 | OK | all ok | all ok | `test_off_linux_is_unmeasurable` (the off-Linux control) |
| ext4 | 17 | OK | all ok | all ok | same |
| XFS | 17 | OK | all ok | all ok | same |

The 9 Isolation tests that passed on each: `test_arguments_are_never_reinterpreted`, `test_caller_mounts_are_unchanged_after_a_clean_exit`, `test_caller_mounts_are_unchanged_after_a_failing_exit`, `test_caller_mounts_are_unchanged_after_a_signal`, `test_command_runs_in_the_canonical_path_in_its_own_namespace`, `test_exit_code_is_forwarded`, `test_signal_is_forwarded`, `test_source_is_visible_through_the_canonical_path_only`, `test_writes_inside_the_namespace_land_as_the_real_user`.
The 7 Refusals tests that passed: `test_both_routes_refused_names_both_in_the_message`, `test_canonical_inside_source_refuses`, `test_missing_canonical_directory_refuses_before_running_the_command`, `test_no_command_refuses`, `test_no_namespace_never_falls_back_to_the_raw_path`, `test_relative_source_refuses`, `test_unknown_ns_mode_refuses`.

Cargo byte identity, N = 6 canonical builds per cell:

| Filesystem | `CARGO_INCREMENTAL` | Files per build | Distinct canonical manifests | Canonical builds differing from build 1 | Native control differing from build 1 |
|---|---|---|---|---|---|
| btrfs | 0 | 43 | 1 | 0 of 5 | 15 |
| btrfs | 1 | 122 | 6 | 5 of 5, 81 paths each | 94 |
| ext4 | 0 | 43 | 1 | 0 of 5 | 15 |
| ext4 | 1 | 122 | 6 | 5 of 5, 81 paths each | 94 |
| XFS | 0 | 43 | 1 | 0 of 5 | 16 |
| XFS | 1 | 122 | 6 | 5 of 5, 81 paths each | 94 |

Reading it:

- With `CARGO_INCREMENTAL=0`, the setting the canonical route now applies, all 6 builds were byte-identical on all three filesystems, across slotA and slotB and across rebuilds.
  That is 43 of 43 files each.
- With `CARGO_INCREMENTAL=1`, the do-nothing control for the setting, every build differed from every other (81 paths) on all three filesystems. 79 are in the random-named `debug/incremental` directory and 2 are `liblib-<h>.rlib` and `liblib.rlib`, as `cargo171.md` found.
  So the identity above is not an artefact of the harness.
- Embedded paths, checked with `grep -aF` on `target/debug/app` on every filesystem: the last canonical build contained the canonical path once and its own slot path zero times.
  The native build contained its own slot path once and the canonical path zero times.
  This is what the namespace is for.
- The native control differs from the canonical builds in 15 paths on btrfs and ext4: `debug/app`, `app.d`, the build-script `root-output` and `.d`, `deps/app-<h>` and `.d`, `lib-<h>.d`, `liblib-<h>.rlib` and `.rmeta`, `libmac-<h>.so`, `mac-<h>.d`, `liblib.d`, `liblib.rlib`, `libmac.d`, `libmac.so`.
  These are the files that embed the build path.
- XFS differs in one more path: `debug/.fingerprint/app-940696a604616385/dep-bin-app`, 16 in total.
  Only the control differs, never two canonical builds.
  Cause, checked by `xxd`: the file lists two dependency entries, `src/main.rs` and `debug/build/app-<h>/out/gen.rs`.
  The native build wrote them in the order gen.rs then main.rs, the canonical build in the order main.rs then gen.rs.
  The same native build was repeated 3 times on XFS and gave the same hash all 3 times, so it is stable per path, not random.
  The entries are the same, only the order differs.
  That the order depends on the build path (for example a hash-ordered set of path strings) is an inference, not isolated: I did not read cargo's source.
  On btrfs and ext4 the native order happened to match the canonical one.

## Row 3: a real treehouse lease

Treehouse binary:

- Obtained as the prebuilt release `treehouse-v3.1.2-linux-amd64.tar.gz` from `https://github.com/kunchenguid/treehouse/releases/download/v3.1.2/`.
- sha256 checked against the release `checksums.txt`: expected and actual both `bc059c6dbbcf6b11a741e92aed1d845d5b4a96f1d9c5e6f78c1aa2502669962d`.
- Installed to `/mnt/docs/Projects/cowfs-171/bin/treehouse`, `treehouse --version` prints `v3.1.2`, the same version as the Mac.

Run (`lease171.sh <out> 3`, started with `nohup`, log polled):

1. `cowfs-daemon --backend path --store S --mount M --socket C` serving a real FUSE mount, as `scripts/namespaces17-treehouse-linux.sh` does.
2. A git repository holding the `cargo171` fixture, one commit `aed2408617eaa1eaa906ffa0fd240e84e7d2478f`, imported once as the seed snapshot.
3. Three times: `cowfs-treehouse --socket C --treehouse-bin <bin> --json base refresh --repo R --ref main --build 'cargo build --workspace' --root <root> --canonical <canon> --ns-helper scripts/cowfs-ns-run.sh`, with `HOME` sandboxed and no `--slot`.
   With `--build` and no `--slot`, the companion runs `treehouse get --lease --json` itself, builds in the leased slot through the namespace helper with `CARGO_INCREMENTAL=0` set by the route, then publishes the base.
4. A fourth real lease taken with `treehouse get --lease --root <root>` and built natively at its own path with `CARGO_INCREMENTAL=0` and no helper, as the control.

Result:

- The three companion-driven leases were slots 1, 2 and 3 under `<root>/.treehouse/repo-082839/<n>/repo`.
  The native lease was slot 4.
  All four paths distinct.
  All 3 refresh JSON reports said `"built_in_slot":true`.
- Each leased `target` had 43 files and no `debug/incremental` directory, while the `CARGO_INCREMENTAL=1` runs above produce 122 files.
  So the route itself set `CARGO_INCREMENTAL=0`, not the script.
- 43 files in each slot's `target`.
  The 3 manifests were identical: 1 distinct manifest of 3.
  Lease 1 against lease 2 and lease 3: 0 differing paths.
- The native lease differed from lease 1 in the same 15 paths as the btrfs row above.
- The control API read back `base status`: `{"pool_id":"repo-082839","snapshot":"repo-082839-base","base_commit":"aed2408617eaa1eaa906ffa0fd240e84e7d2478f","head_commit":"aed2408617eaa1eaa906ffa0fd240e84e7d2478f","fresh":true,...}`.
  So a warm base was published through the lease route with provenance.
- The host mount table, excluding the daemon's own FUSE mount, was identical before and after.
  The canonical directory was empty before and after.
- Cleanup: the daemon was shut down through `cowfs shutdown` and the FUSE mount was gone.
  `treehouse status` showed all 4 slots `leased` after the run.
- `treehouse return <slot>` without `--force` refused all 4, because each slot has an untracked `target` directory and stdin was not a terminal.
  They were then returned with `treehouse return --force <slot> --root <root>`, which cleans a slot.
  These slots were created by this run under its own root, so nothing else was cleaned.
  `treehouse status` then showed all 4 `available`.

## Commands and privilege

- Sudo was used only through `fsimg.sh`, which reads the password from stdin once and runs `sudo -S -p ""` on exactly: `losetup --find --show`, `mkfs.ext4 -q -F`, `mkfs.xfs -q -f`, `mount`, `chown` (of the mount root to the normal user), `umount`, `losetup -d`.
  All on images under `/mnt/docs/Projects/cowfs-171/img`.
- The password was read from the repo `.env` at run time on the Mac and piped to ssh on stdin: `printf '%s\n' "$P" | ssh zeeshan@100.122.64.51 '<workspace>/fsimg.sh up|down ext4|xfs'`.
  It is not in this document, a file, a commit or a log.
  Deviation from the box rules: the four Mac-side calls also piped ssh's output through a local `sed` that took the password as its pattern, so the value was briefly in that local `sed` argv on the Mac.
  It never reached the box command line.
  `fsimg.sh` never echoes the password, so that redaction was unnecessary and was not used again.
- After the ext4 and XFS runs, `umount` and `losetup -d` ran and the images were deleted.
  Verified: `mount | grep cowfs-171` printed nothing (exit 1), `losetup -a` printed nothing, `ls img/` empty.
  No cowfs, cargo, rustc or treehouse process was left running.
- The unprivileged route (`unshare --user --map-root-user --mount`) was used for every namespace in this document.
  No test or build was run as root.

## What is NOT covered

- Older kernels.
  This box has one kernel, 7.2.8-2-cachyos.
  The earlier moonscape measurement was 6.12.
  Nothing here covers any older kernel and none is claimed.
- The ext4 and XFS filesystems were loop images backed by a btrfs file, so their storage stack is not a native block device.
  Behaviour was exercised at the filesystem layer, not at the device layer.
  The btrfs row used `compress=zstd:1`.
- The cowfs FUSE mount was never the filesystem under test in the matrix: the matrix builds ran on the plain filesystems.
  The FUSE mount was in the lease run, but only to hold the store; the leased slots sat on btrfs, not on the FUSE mount.
- The lease run used the path backend, not the core backend, and one pool, one fixture repository, one commit.
- One workspace (three small crates, no registry dependencies), one toolchain (rustc 1.99.0), debug profile, clean rebuilds only.
  Release builds, registry dependencies, `cargo test`, incremental rebuilds over an existing `target`, and `CARGO_TARGET_DIR` outside the snapshot were not measured.
- N is 6 builds per cell (3 leased builds in the lease run).
  Zero differences in 6 is a sample, not proof of zero.
- The isolation tests ran once per filesystem (0.33 to 0.35 s).
  They are fast and were not repeated for flake.
- The XFS `dep-bin-app` ordering difference is explained from the bytes, not from cargo's source.
- Not run: the full `linux-fuse` and `check` CI jobs, which are covered by CI.
- A leased slot that is itself a cowfs snapshot on the FUSE mount is still unmeasured.
  The leased slots here were plain treehouse worktrees on btrfs.
- `cargo171.md` says its Linux host used ext4 for `/mnt/docs/Projects/cowfs-cargo171`.
  On this box `/mnt/docs` is btrfs (`findmnt` at the start of this run).
  That label in `cargo171.md` looks wrong.
  It is left as is here and reported as a follow-up.
