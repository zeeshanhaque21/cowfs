# Linux mount namespaces for canonical clone paths

Issue #17.
Linux only.
The helper is `scripts/cowfs-ns-run.sh`, its contract tests are `bench/test_namespaces.py`, and the end-to-end run is `scripts/namespaces17-linux.sh`.

## What this is for

A build artifact records the absolute path it was built at.
Two agents working in two snapshots therefore produce two different artifacts from identical source, which is the cost spike 6 measured at 12.8% to 28% of the first slot for a default debug build.

`docs/design.md` settles the Linux answer: optional per-agent mount namespaces that place every clone at an identical canonical path.
macOS gets `--remap-path-prefix` instead, because it has no FUSE mount namespaces to unshare.

Spike 6 also found namespaces are not needed for dedup on mode (b), where a warm base is copied to a new path and stays Fresh.
They matter for mode (a), where unmodified treehouse builds independently at different paths.

## The contract

    cowfs-ns-run.sh --src DIR --canonical DIR [--ns-mode auto|unprivileged|privileged] -- CMD [ARG...]

`--src` is the snapshot directory as the caller sees it.
`--canonical` is an existing directory, the same string for every agent on the host.
The command runs with `--src` visible at `--canonical`, with its working directory there, and with nothing else changed.

Exit codes:

| Code | Meaning |
|---|---|
| the command's own | forwarded verbatim, including the signal it died from |
| 2 | usage: a missing, relative, non-directory or nested path, or no command |
| 77 | UNMEASURABLE: no namespace, so nothing ran |

Anything other than 0 or the command's own code means the command did not run.

## How the isolation is obtained

`unshare` does all of it.
No library, no daemon, no setuid helper, no dependency.

    unshare --user --map-root-user --mount --propagation private -- <payload>

The payload re-executes this script in `__inner` mode, which does the three things the namespace exists for:

- `mount --rbind -- SRC CANONICAL`, then `mount --make-private CANONICAL`.
  The second call matters: `--rbind` copies the source's propagation flags across, and a shared mount would send this namespace's mounts back to the caller's peers.
- `cd CANONICAL`, then a `pwd -P` check that the resolved path is exactly the canonical string.
  A canonical path that contains a symlink would silently record the resolved path instead, so it is refused.
- `exec "$@"`, so the command inherits the exit code, the fatal signal and the terminal, with nothing in between.

`--propagation private` is what makes the namespace's mounts invisible to the caller: it clears the shared flag on the whole tree before anything is mounted inside it.

The unprivileged route is tried first because it needs no privilege at all, and `--ns-mode privileged` is the fallback for a caller that already has `CAP_SYS_ADMIN`.
The helper never calls `sudo`, never installs anything, and never changes a sysctl or a capability.

## Failing closed

The helper probes before it commits.
It runs the payload in a throwaway namespace, reads `/proc/self/ns/mnt` there, and compares that id with its own.
Only a different id counts as a namespace.

If no route produces one, the helper prints a line containing `UNMEASURABLE`, exits 77, and does not run the command.
There is no fallback path that runs the command at its raw path, because a build that quietly ran unwrapped produces artifacts that look comparable and are not.

Off Linux the same refusal happens, with the platform named in the message.

## Security properties

| Property | How it is obtained | Checked by |
|---|---|---|
| The caller's mounts never change | A separate namespace, with propagation private before any mount inside it | `test_caller_mounts_are_unchanged_after_a_clean_exit`, `..._after_a_failing_exit`, `..._after_a_signal` |
| No mount leaks on a signal | The namespace dies with the last process in it | `test_caller_mounts_are_unchanged_after_a_signal` |
| The canonical directory stays empty outside the namespace | Only the namespace binds onto it | `test_source_is_visible_through_the_canonical_path_only` |
| Files written inside stay owned by the real user | The kernel maps uid 0 in the user namespace back to the caller's uid | `test_writes_inside_the_namespace_land_as_the_real_user` |
| No argument is reinterpreted | argv throughout, no `sh -c`, no string built from input | `test_arguments_are_never_reinterpreted` |
| No raw-path fallback when a namespace is refused | Probe failure exits 77 with the command unrun | `test_no_namespace_never_falls_back_to_the_raw_path` |
| Both refused routes named in the message | Every candidate's reason is kept, not just the last | `test_both_routes_refused_names_both_in_the_message` |
| No capability, sysctl, or package change | `unshare` and `mount` only | read of the helper |

The three mount checks each mount a tmpfs inside the namespace first and assert the child's own mount count went up, so "the caller's mounts did not change" cannot pass because the child failed to mount anything.

## Measured on a real cowfs FUSE mount

Run on `moonscape` (Debian 13, kernel 6.12.109, aarch64) as an unprivileged user with no `sudo`.
Full log and artifacts: `bench/out/namespaces17/`.

A private cowfs daemon with its own store, its own mount point and its own control socket, all under `bench/out/namespaces17`, mounted `fuse.cowfs` at `mnt`.
A one-file Rust crate was ingested into `base`, verified by blake3 on import, and cloned into two fresh snapshots `slotA` and `slotB`.

Each snapshot was built with `rustc -g --edition 2021 main.rs -o app` inside a namespace, at the same canonical path:

| Artifact | Built at | SHA-256 (head) | Bytes |
|---|---|---|---|
| A1 | canonical, `slotA` | `d10a6ec0b048a8c9e70d18ae04cb0e895b6da2fa043ecf3b0266dfeb907138b2` | 4370016 |
| A2 | canonical, `slotA` again | `d10a6ec0b048a8c9e70d18ae04cb0e895b6da2fa043ecf3b0266dfeb907138b2` | 4370016 |
| B1 | canonical, `slotB` | `d10a6ec0b048a8c9e70d18ae04cb0e895b6da2fa043ecf3b0266dfeb907138b2` | 4370016 |
| N1 | `mnt/slotA`, its own path | `0a8ba3cdad45786a5f56daa0b8c5bf519ce36b15fedd46139031caf39794f7a8` | 4370016 |

Three claims, each against its own control:

- A1 and B1 are byte-identical, so two different snapshots at one canonical path produced one artifact.
- A1 and A2 are byte-identical, so the equality is not luck about rebuilds: a same-path rebuild is identical too, which is what makes the cross-snapshot equality meaningful.
- N1 differs from A1 in 29 bytes out of 4370016, and it records its own slot path where A1 records the canonical one, so the native control really did build somewhere else.

`bench/out/namespaces17/embedded-paths.txt` shows the path strings read back out of each binary: `canonical_path_embedded=True` for A1, A2 and B1, `slot_path_embedded=True` for N1.

The caller's mount table was byte-identical before and after the three canonical builds (`mountinfo-before.txt` and `mountinfo-after.txt`), and the canonical directory was empty before and after.

`rustc` itself is deterministic for a fixed path and a fixed argv, which is what makes A1 against A2 a usable control.
`cargo` is not: spike 6 measured same-path debug rebuilds at 92% identical, because the incremental query cache and the proc-macro dylib carry run-specific bytes.
So this run used `rustc` directly, and a canonical path says nothing about a cargo build that also embeds run-specific bytes.

## Limits

- Linux only, by design. macOS gets `--remap-path-prefix`.
- One canonical path per host per namespace.
  Two agents cannot both be at the same path at the same time outside a namespace, so each needs its own canonical string if they run at the same instant.
  Nothing here coordinates that; the caller picks the strings.
- The canonical directory must already exist.
  The helper creates nothing outside the namespace, so provisioning it is the caller's job.
- `--canonical` inside `--src`, or the reverse, is refused.
- The command inherits the caller's environment unchanged.
  The helper sets nothing and removes nothing, so `TMPDIR`, `CARGO_TARGET_DIR` and friends reach the command as the caller set them.
- The namespace is per command, not per session.
  A shell started inside one keeps it; a new command gets a new one.
- Nothing here is wired into `cowfs-treehouse` yet.
  The execution seam is `crates/cowfs-treehouse/src/mode_b.rs:550` `run_build`, which currently runs a build command through `sh -c` in the slot directory. Wrapping that one call site is the natural integration, and it is deliberately not in this change: the canonical directory has to be chosen by whoever owns the pool, and that choice does not belong in a helper.
- Verified on one kernel (6.12) and one filesystem (ext4 under the mount, `fuse.cowfs` for the source).
  Not verified on btrfs, XFS, or an older kernel.

## Running it

The contract tests, which run everywhere including macOS and report which branch they took:

    python3 -m unittest discover -s bench -v

On a host without namespaces, the isolation tests are skipped with the reason, and the refusal tests run.
That is a pass for the refusal path, not for isolation, and the run says so in its first line.

The GitHub `ubuntu-latest` runner is such a host: its kernel refuses `CLONE_NEWUSER` for an unconfined process, so CI runs the refusal matrix there and reports `OK (skipped=10)`.
That is the honest outcome and not a CI gap in the tests.
The isolation matrix was run on `moonscape` instead, and the measurements above are from there.
`test_both_routes_refused_names_both_in_the_message` puts a refusing `unshare` stub first on `PATH`, so the two-route refusal message is still checked everywhere, including on a host where a namespace does work.

The end-to-end run, on a Linux host with `/dev/fuse` and a Rust toolchain:

    scripts/namespaces17-linux.sh

It builds `cowfs-cli` into its own output directory, starts a daemon it owns, ingests a fixture, clones two snapshots, runs the three canonical builds and the native control, and rules.
Its verdict is only ever PASS or UNMEASURABLE.
It signals only the daemon it started, and only after checking that process's command line names its own store.