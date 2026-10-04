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

Exit code 77 is ambiguous by design: a command whose own exit code is 77 is byte-identical, from the outside, to the helper's refusal.
Do not classify on the code alone.
Probe the helper once with a command you expect to succeed, and use that to tell the two apart; `scripts/namespaces17-linux.sh` does exactly this in `probe_namespace`.

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

## What the canonical directory does and does not contain

The namespace makes one directory point at the snapshot.
It does not make the command's filesystem private.

Inside the namespace the command still shares the caller's `/tmp`, `/usr`, `/home` and every other absolute path outside the snapshot, so a write there reaches the real filesystem exactly as it would without the helper.
Only the snapshot's own path is redirected.
That is inherent to a mount namespace, and it is not what "nothing else changed" in the contract means: read that as "nothing else about the mounts changed".

Any existing directory is accepted as `--canonical`, including a system path: `--canonical /sys` succeeds, and inside the namespace the snapshot shadows `/sys`.
Nothing on the host changes, but a caller who passes `/usr` gets a build whose `/usr` is the snapshot, with no warning and no nonzero exit.
There is no list of refused prefixes, because any such list is wrong somewhere and a caller who needs one should not be relying on a helper for it.

The safe rule is the caller's: pass an existing, empty, per-pool directory that belongs to that pool.

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
- N1 differs from A1, and it records its own slot path where A1 records the canonical one, so the native control really did build somewhere else.
  The byte count is a property of this path pair, not of the mechanism: A1 and N1 are the same size (4370016 bytes each) only because `canonical` and `mnt/slotA` are both 9 characters, and the 29 differing bytes are where those two equal-length strings sit.
  Pick suffixes of different lengths and the sizes differ too.

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
- The treehouse wiring uses `--slot`, not a treehouse lease.
  moonscape has no `treehouse` binary, so the integration run builds in the snapshot itself.
  That is the same `run_build` call site a leased slot takes; only the slot provider differs.
- The integration run uses the path backend, not the core backend, because `base_refresh` copies a
  directory into the store and the core backend refuses that by design.
  It is the backend that supports the operation under test, serving the same store over the same
  FUSE mount.
  Block-level verification is the core backend's, and belongs to the helper run.
- On git 2.39, `base_refresh` fails after the build with `git worktree add did not report where it
  worked`, because 2.39 puts `HEAD is now at <sha>` on stdout where 2.56 puts the path.
  That is `crates/cowfs-daemon/src/import.rs`, outside this change.
  The integration run reports it and judges the refresh by whether its artifact landed.
- Verified on one kernel (6.12) and one filesystem (ext4 under the mount, `fuse.cowfs` for the source).
  Not verified on btrfs, XFS, or an older kernel.

## Running it

The contract tests, which run everywhere including macOS and report which branch they took:

    python3 -m unittest discover -s bench -v

On a host without namespaces, the isolation tests are skipped with the reason, and the refusal tests run.
That is a pass for the refusal path, not for isolation, and the run says so in its first line.

`bench/test_namespaces.py` holds 17 tests: 8 refusals that run everywhere, and 9 isolation tests
that need a namespace.
On macOS: `Ran 17 tests`, `OK (skipped=10)`, the 8 refusals pass and the 9 isolation tests plus
`test_both_routes_refused_names_both_in_the_message` are skipped.
On real Linux with a namespace: `Ran 17 tests`, `OK (skipped=1)`, all 9 isolation tests run and the
two-route check passes.
The full `bench/` suite reports 53 tests on macOS, 10 of them skipped, and the rest of them belong to
`bench/test_gates.py`.

The GitHub `ubuntu-latest` runner denies namespaces, so CI takes the macOS branch and reports
`OK (skipped=10)` there.
That is the honest outcome and not a CI gap in the tests: a refusal pass is not an isolation pass, and
the run's first line says which branch it took.
The isolation matrix was run on `moonscape` instead, and the measurements above are from there.
`test_both_routes_refused_names_both_in_the_message` puts a refusing `unshare` stub first on `PATH`, so the two-route refusal message is still checked everywhere, including on a host where a namespace does work.

The end-to-end run, on a Linux host with `/dev/fuse` and a Rust toolchain:

    scripts/namespaces17-linux.sh

It builds `cowfs-cli` into its own output directory, starts a daemon it owns, ingests a fixture, clones two snapshots, runs the three canonical builds and the native control, and rules.
Its verdict is PASS, FAIL or UNMEASURABLE: a real failure of the product is FAIL, and only a missing prerequisite is UNMEASURABLE.
It signals only the daemon it started, and only after checking that process's command line names its own store.

The wiring run, the acceptance for the treehouse integration rather than for the helper:

    scripts/namespaces17-treehouse-linux.sh

It starts a real `cowfs-daemon` over a real FUSE mount, makes a real git repo, and drives the real
companion through `base refresh --build --canonical --ns-helper`.
`crates/cowfs-treehouse/tests/canonical.rs` covers the rest: default compatibility, the refusals, the
flag pairing, argv integrity, the refused-namespace case and the payload-77 collision.
It holds 13 tests on Linux and 10 on macOS, because 3 are behind `cfg(target_os = "linux")`.
Those 3 are the ones that exercise a live namespace decision, and they are exactly the ones macOS
cannot run, which is why the first version of them passed locally and then failed on the CI ubuntu
runner: the stubs wrote to an unquoted path in a shell script, and the temp directory name carried a
parenthesis from Rust's `ThreadId` formatting.
A path in a generated script is quoted, and the temp directory name is shell-safe.
Those are stub-only and prove the wiring rules, not the namespace.
The stub-only tests run in CI; the namespace integration does not, because CI's runner refuses
`CLONE_NEWUSER`.

## The treehouse wiring

`run_build` at `crates/cowfs-treehouse/src/mode_b.rs:638` takes an optional `Canonical`.
Both existing call sites pass it, and there is no third code path.

    cowfs-treehouse base refresh --repo R --build CMD --canonical DIR --ns-helper scripts/cowfs-ns-run.sh

Absent, the build runs at the slot's own path exactly as before, so existing macOS and Linux behaviour
and every existing config file are unchanged.
Present and Linux, the launch becomes the helper with `--src <slot> --canonical DIR -- /bin/sh -c CMD`.

`/bin/sh -c` is kept for the command, because that command is user configuration and has always been
a shell string.
It is passed as one argv element, and the canonical directory is a separate argv element, so neither is
re-split or concatenated.
A canonical directory containing a space or a quote cannot reach a shell, which
`the_canonical_path_is_never_pasted_into_the_command_string` pins.

`--canonical` and `--ns-helper` go together; either alone is exit 2.
The helper path is given explicitly because a distributed binary cannot assume a working directory.
`validate` refuses, as usage errors, a relative canonical path, a canonical directory that does not
exist, and a helper that is not a file, then refuses a non-Linux platform as Unsupported.
The companion never creates the canonical directory and never guesses a system path for one.

One probe runs before each build, with a command that must succeed, because 77 is the helper's own
refusal code and also a payload's own exit code.
A failed probe is Unsupported carrying the UNMEASURABLE text; a payload that exits 77 stays an Io
failure.
The probe is why the ambiguous code is harmless here, and
`a_passing_payload_seventy_seven_stays_a_failure` is what pins it.

## Measured through the wiring

Run on moonscape, path backend, real FUSE mount, real daemon, real companion.
Artifacts: `bench/out/namespaces17-treehouse/`.

| Artifact | Built at | SHA-256 | Bytes |
|---|---|---|---|
| base | canonical, the warm base | `90c90a2c7e1428c3bc8fcb3804de65c3113f2bd7884ed49fbaa1ee43e3c7aba7` | 4370032 |
| slotA | canonical, fresh clone of base | `90c90a2c7e1428c3bc8fcb3804de65c3113f2bd7884ed49fbaa1ee43e3c7aba7` | 4370032 |
| slotB | canonical, fresh clone of base | `90c90a2c7e1428c3bc8fcb3804de65c3113f2bd7884ed49fbaa1ee43e3c7aba7` | 4370032 |
| N-slotA | its own path, do-nothing baseline | `b6fa6424206ce8daf6ba5e3a9cd658173ae146073586bdfe85b75e08b5c88b81` | 4370032 |
| N-slotB | its own path, do-nothing baseline | `42f1926fe7c5d1e8ad969bf460e7d47bfd14ba25f77a3296bbbc688360b05a45` | 4370032 |

The warm base and both fresh slots came out as one artifact, and each native control at its own slot
path came out different, so the control really built elsewhere.
`embedded-paths.txt` reads the path strings back out of all five binaries.

The store was then reloaded from disk: the daemon was stopped, the same store mounted again, and
`base`, `slotA` and `slotB` all served `main.rs` at
`7fa626e8bff724acfb0ed8b61a8cc21ec2720db1ff08da4a6746b8db6826813d`, with every artifact present.
That is a restart readback, not crash injection, and no no-data-loss claim is made from it.