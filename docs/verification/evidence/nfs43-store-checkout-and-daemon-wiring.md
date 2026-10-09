# Store-mode sidecars after a checkout, and the dead-server hang wiring

Worktree: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/7/cowfs`, branch `test/nfs-store-checkout-43` from main `460e1ae`.
Commit `dffae94`, one new test file, no source change.
Covers issue #43 bullet "`Store` mode leaves untracked `._` files after a checkout; nothing asserts on it".

## Task A: what Store mode actually does

`AppleDoubleMode` is declared in `crates/cowfs-nfs/src/adapter.rs:122`.
The three arms are `Translate`, `Hide` and `Store`.
`Store` is documented at `adapter.rs:132` as "Treat `._` names like any other name", and that is literally all it is.
Every mode-conditional in the adapter is a comparison against `Hide` or `Translate`:

* `Adapter::translating` (`sidecar.rs:132`) returns false unless the mode is `Translate`, so nothing is a view.
* `Adapter::readdir` (`adapter.rs:925`) skips a `._` entry only when the mode is `Hide`, so `Store` lists them.
* `Adapter::remove` (`adapter.rs:749`) removes `name`'s sidecar only when the mode is `Hide`, so `Store` leaves it.
* `Adapter::rename` (`adapter.rs:857`) moves `name`'s sidecar only when the mode is `Hide`, so `Store` leaves it at the old name.
* `Adapter::rmdir` (`adapter.rs:803`) purges a directory that holds only sidecars only when the mode is `Hide`.
* `Adapter::side_write` (`sidecar.rs:314`) is the only place `is_plausible_prefix` can refuse bytes, and it is on the sidecar path, so in `Store` any bytes under a `._` name are stored.

So the derived behaviour is: after a checkout, `._` entries are real stored files with their own inodes, their exact written bytes come back on READ, they are listed by READDIRPLUS, no xattr is taken from or given to the main file, REMOVE of the main file leaves the sidecar behind as an orphan, RENAME neither moves nor copies it, and a `._` name with no main file is the same ordinary file as any other.

### What the test asserts

`crates/cowfs-nfs/tests/store_checkout43.rs`, raw-protocol harness (`tests/common/mod.rs`, a `MemVfs` served in process over TCP), so it runs on Linux CI with no real mount and no macOS.

`store_mode_keeps_sidecars_as_real_files_after_a_checkout`
Creates `file`, writes `checked out content`, then creates `._file` and writes `sidecar_with("user.color", "blue")` bytes, the same helper shape as `tests/translate.rs:16`.
Asserts `c.names(&root) == ["._file", "file"]`, that `._file` resolves in the `Vfs` to a distinct inode whose size is the sidecar length, that READ returns the exact bytes written and that `Sidecar::decode` finds `user.color = blue`, that the main file still reads its own content and has an empty xattr list, and that non-sidecar bytes under `._junk` are accepted and read back verbatim.

`store_mode_leaves_the_sidecar_behind_when_the_main_file_is_removed`
Asserts REMOVE of `file` gives `Error::NotFound` for `file` in the `Vfs` while the directory still lists `._file` and READ still returns the sidecar bytes.
Asserts RENAME `other` to `renamed` and back to `file` produces no `._renamed` and leaves `._file` untouched.
Asserts the sidecar is still a real file and REMOVE of it works.

`store_mode_treats_a_sidecar_with_no_main_file_as_an_ordinary_file`
Puts `._x.txt` in `__MACOSX` with no `x.txt` beside it.
Asserts it is stored under its own name, READ returns the exact bytes, it is listed, `x.txt` does not exist and nothing was taken from a main file, and REMOVE of it works.

Every helper and status constant used was read out of `tests/common/mod.rs` before use: `serve`, `memfs`, `Nfs::{create_file, write, read, names, remove, rename, mkdir, lookup}`, the `OK`/`NOENT` constants and `NOENT`.
`Sidecar::{from_xattrs, encode, decode}` are the public `pub` items of `crates/cowfs-nfs/src/appledouble.rs`, re-exported as `cowfs_nfs::Sidecar` at `crates/cowfs-nfs/src/lib.rs:113`.
Nothing private was needed and nothing was invented.

`rustfmt --edition 2021 --check crates/cowfs-nfs/tests/store_checkout43.rs` passes (exit 0, after one format pass).

## Task B: dead server hang cure, call sites

Read-only, by grep across `crates/` (the codebase-memory project index for this worktree path is stale, so grep was used for these two symbol names).

`install_signal_cleanup`

* `crates/cowfs-nfs/src/cleanup.rs:39` definition, re-exported at `crates/cowfs-nfs/src/lib.rs:114`.
* `crates/cowfs-nfs/src/mount.rs:384` `Mount::install_signal_cleanup` wrapper.
* `crates/cowfs-daemon/src/mounts.rs:111` `mounts::install_signal_cleanup`, macOS arm calls `cowfs_nfs::install_signal_cleanup()` at `mounts.rs:114`, Linux arm calls `cowfs_fuse::Mount::install_signal_cleanup()` at `mounts.rs:118`.
* `crates/cowfs-daemon/src/daemon.rs:269` `install_backstop_signal_cleanup`, the only caller of `mounts::install_signal_cleanup`, re-exported at `crates/cowfs-daemon/src/lib.rs:50`.
* `crates/cowfs-fuse/src/mount.rs:258` and `crates/cowfs-fuse/src/lifecycle.rs:126`, plus `crates/cowfs-fuse/examples/mounthost.rs:12` which does call it.
* Tests: `crates/cowfs-nfs/tests/mount.rs:653`.

`sweep_stale_mounts`

* `crates/cowfs-nfs/src/cleanup.rs:106` definition, re-exported at `crates/cowfs-nfs/src/lib.rs:114`.
* `crates/cowfs-daemon/src/mounts.rs:91` `mounts::sweep_stale`, macOS arm calls `cowfs_nfs::sweep_stale_mounts` at `mounts.rs:94`, Linux arm calls `cowfs_fuse::sweep_stale_mounts` at `mounts.rs:101`.
* `crates/cowfs-daemon/src/daemon.rs:260` `prepare_platform`, which loops `mounts::sweep_stale(mount_prefix)` at `daemon.rs:261` and logs each path.
* `crates/cowfs-daemon/src/main.rs:55` `prepare_platform(&config.mount)` before `Daemon::start`.
* `crates/cowfs-daemon/src/daemon.rs:223` `open_handler` also calls `prepare_platform`, so `cowfs serve` through `crates/cowfs-cli/src/backend.rs:76` sweeps too.
* `crates/cowfs-fuse/src/lifecycle.rs:91` definition, re-exported at `crates/cowfs-fuse/src/lib.rs:134`.
* Tests: `crates/cowfs-nfs/tests/mount.rs:815,827,1245`, `crates/cowfs-fuse/tests/mount.rs:859,861`, and the unit test `crates/cowfs-daemon/src/mounts.rs:296`.

### Does cowfs-daemon call both at startup and shutdown?

Sweep: yes, at startup, on both entry paths.
`cowfs-daemon` the binary calls `prepare_platform` at `main.rs:55` before anything is mounted, and `open_handler` (`daemon.rs:223`) calls it again for the library path used by `cowfs serve`.
Both go to `cowfs_nfs::sweep_stale_mounts` on macOS.

`install_signal_cleanup`: deliberately not called by the daemon, and that is a recorded decision rather than a gap.
`crates/cowfs-daemon/src/daemon.rs:252-259` states the reason: the platform handler unmounts and then calls `process::exit(128 + sig)` from its own signal thread, which races the daemon's ordered shutdown and usually wins it, leaving the control socket on disk.
The daemon instead installs its own handler in `main.rs:85` `watch_signals` (SIGINT and SIGTERM, first signal calls `daemon.stop()`, second exits 130) and `Handler` unmounts every export and the default mount in order.
`docs/v1-daemon.md:27,51` documents the same.

Exact missing wiring: none for the daemon, and none for `cowfs serve` either, because `open_handler` sweeps and the CLI path inherits it.
What does not exist is any production caller of `cowfs_daemon::install_backstop_signal_cleanup`.
It is defined (`daemon.rs:269`), re-exported (`lib.rs:50`) and called from nowhere in `crates/`.
Its stated purpose, a process that mounts and does nothing else, is served only by `crates/cowfs-fuse/examples/mounthost.rs:12`, which is the FUSE example and is Linux-only, so on macOS no shipped binary installs the NFS signal backstop.
If a macOS-only host process is ever added, the one line it needs is `cowfs_daemon::install_backstop_signal_cleanup()?` before its first `Mounted::mount`.
Nothing was implemented here.

## Limits

* No local `cargo build` or `cargo test` was run, by instruction (disk and artifact limits).
  The test has never been executed anywhere.
* CI is the first execution. The three tests assert behaviour derived by reading `adapter.rs`, `sidecar.rs`, `appledouble.rs` and `tests/common/mod.rs`; if CI disagrees, the source reading was wrong and the test is the finding.
* `rustfmt --check` was run locally and passes. Lint and clippy were not run.
* Task B is grep and read only. No process was started, killed, mounted or swept.
* Only `crates/cowfs-nfs/tests/store_checkout43.rs` was created. No source file, no other test file and no other worktree was touched.