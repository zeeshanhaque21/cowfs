# Verification: issue #43, NFS Translate namespace and sidecar-view guards

Lease 7, branch `fix/nfs-translate-security-43`, base `46b0f269d5bef4a2c204c25f5b3015da601d3beb`.

Issue #43 asks a round-3 critic to review the Translate and security code added after the round-2
report (`spikes/nfs-loopback/out/critic12b/report.md`), and to wire the dead-server cure into the
daemon.
This document records what was reproduced, what was found already correct, and what was changed.
Raw ignored output: `bench/out/ready-43/`.

## Summary

One caller-reachable defect with silent data loss, found and fixed.
Three security items in the issue were checked and are already correct as shipped, with evidence.
The dead-server wiring the issue asks for is already in the daemon, with a recorded reason why its
`install_signal_cleanup` is deliberately not installed.
Two items are unmeasurable on this machine and are reported as such rather than closed.

## The defect: a real object could take a live sidecar name

`._name` is not an ordinary name while `name` exists.
In `Translate` mode it is a *view* of `name`'s extended attributes and has no inode of its own.
`CREATE` already routed such a name to the view, and `RENAME` already refused to move a real file
onto one.
`MKDIR`, `SYMLINK` and `LINK` did not check at all, so a real directory, symlink or hard link could
take the name.

### Reproduction, raw NFSv3 over TCP against the in-process server

`crates/cowfs-nfs/tests/common` is a raw ONC RPC client: it builds real RPC record frames, sends
them over a real `TcpStream` to the real listener, and reads the real replies. No mount, no
`Adapter` shortcut. The pre-fix run of the three new tests in `tests/translate.rs`:

```
test result: FAILED. 10 passed; 3 failed; 0 ignored

test a_view_cannot_be_made_a_real_object ... FAILED
  assert_eq!(c.mkdir(&root, "._doc").0, ACCES, "a directory")
    left: 0        (OK)
   right: 13       (ACCES)

test a_link_under_a_view_name_never_rewrites_the_files_content ... FAILED
  assert_eq!(c.link(&doc, &root, "._doc").0, ACCES, "refused")
    left: 0        (OK)
   right: 13       (ACCES)

test a_dot_name_view_is_noent_and_the_name_is_a_real_file ... FAILED
  assert_eq!(c.lookup(&root, "._.").0, NOENT)
    left: 22       (INVAL)
   right: 2        (NOENT)
```

### The silent data loss, step by step

A throwaway probe (`tests/zz_probe.rs`, deleted after the run) drove the exact sequence the macOS
kernel uses to set an extended attribute: create `._doc`, write a whole AppleDouble file to it,
commit.
Observed on the unmodified base, every call returning `OK`:

```
doc ino=2 size=Ok(19)                       content: "the quick brown fox"
link(doc, "._doc")            -> st=0       ._doc is now the same inode 2, nlink=2
create("._doc", mode=0644)    -> st=0       returns doc's own handle
write(handle, 0, 4096 bytes)  -> st=0
commit(handle)                -> st=0
doc now: size=Ok(4096)
  content = [0, 5, 22, 7, 0, 2, 0, 0, "Mac OS X        ", ..., 255]
names(root) = ["._doc", "doc"]
```

`doc`'s 19 bytes of content were replaced by a 4096-byte AppleDouble file, and no call reported an
error.
`nlink` went to 2, so `rm ._doc` would have removed a name, not the data.

The same probe showed the other two shapes, both of which break the file's attribute channel
permanently:

```
--- mkdir(._doc)  -> st=0   real ._doc is a Directory
  create(._doc) -> 21 (ISDIR)      write sidecar -> 21      doc xattrs = []
--- symlink(._doc) -> st=0  real ._doc is a Symlink
  create(._doc) -> 0               write sidecar -> 22 (INVAL)   doc xattrs = []
```

So after one `mkdir ._doc`, `xattr -w` on `doc` cannot work for the rest of the mount: the kernel
writes its sidecar to a name that is a directory.

## The fix

One guard, at the seam that was missing it, plus one status correction.

`crates/cowfs-nfs/src/adapter.rs`, a new `Adapter::not_a_view` called by `mkdir`, `symlink` and
`link` after `new_name`:

```rust
fn not_a_view(&self, dir: Ino, name: &[u8]) -> NfsResult<()> {
    if self.side_of(dir, name) {
        Err(nfsstat3::NFS3ERR_ACCES)
    } else {
        Ok(())
    }
}
```

`ACCES` is the status `rename` already returns for the same situation ("a real file cannot be moved
onto the view of another file"), so the adapter answers one way for one condition.

`side_of` short-circuits on the mode, so outside `Translate` the guard costs no `Vfs` call at all.

`crates/cowfs-nfs/src/sidecar.rs`, `side_target`: a main name of `.` or `..` now answers `NOENT`.
Those are the two names a `Vfs` refuses to look up and no client can create, so `._.` and `._..`
are real files, and looking one up is a missing name rather than a bad one.
The check is on the name, not on the error, so a `._a/b` still answers `INVAL` as it should.

### What the fix deliberately does not change

* `CREATE` on a live sidecar name still returns the view. That is the path the kernel uses to set
  an extended attribute, and `a_link_under_a_view_name_never_rewrites_the_files_content` exercises
  exactly that sequence after the refusal.
* A stored `._name` file still wins over the view, so a zip, tar or `git checkout` that delivers
  `._x` before `x` keeps a real file. `a_view_cannot_be_made_a_real_object` asserts `EXIST`, not
  `ACCES`, for the three calls against such a name, which is what proves the guard did not fire.
* A name with no main file is still an ordinary name, so `mkdir ._lonely` still works.
* `Hide` and `Store` are untouched. `outside_translate_every_dot_underscore_name_is_an_ordinary_name`
  is the negative control and asserts all three calls still make a real object in both modes.

## Real mount, native tools

`native_tools_cannot_shadow_a_live_sidecar_name` in `tests/mount.rs` runs a real `mount_nfs` of a
`MemVfs` in `Translate` mode and drives `printf`, `xattr`, `mkdir`, `ln -s` and `ln` from `/bin/sh`.
It asserts the mount point is in `/sbin/mount` first, so the log cannot be read as a pass that
never mounted anything. The transcript:

```
MOUNT localhost:/cowfs-a050da596839e1207d0abb9d9d17c793 on
      /private/var/folders/np/.../cowfs-nfs-nwJIbZ/mnt (nfs, nodev, nosuid, mounted by zeeshanhaque)
content=[the quick brown fox] user.k=[v]
refused: mkdir ._doc -> mkdir: ._doc: File exists
refused: ln -s doc ._doc -> ln: ._doc: File exists
refused: ln doc ._doc -> ln: ._doc: File exists
after: content=[the quick brown fox] xattrs=[com.apple.provenance user.k user.j ]
listing: [. .. doc ]
```

`doc` kept its 19 bytes and both attributes, the listing has no `._` entry, and
`CountingVfs::appledouble_names()` is empty, so no sidecar inode was stored.

**Reachability, stated precisely.** The errno is `File exists`, not `Permission denied`, so on this
machine the macOS client refused all three attempts in its own AppleDouble layer before any RPC
reached the adapter. The `ACCES` guard is therefore defence in depth against a client that does not
have that layer: a raw-protocol client, another NFS implementation, or anything speaking NFSv3
directly. That is a real boundary, because the adapter is what a client reaches, and the server's
stated model is that a client may send any legal NFSv3 call. The raw-RPC tests above are what
exercise `ACCES`; the mount test proves the end-to-end safety property on a real mount, not the
guard specifically.

Before the fix, the same two-call sequence over raw RPC corrupted the file. Whether a macOS kernel
would have reached that code path was not established and is not claimed.

## Checked and already correct

Each of these is a named item in #43.
Each was read and, where a test existed, run.

| Item | State | Evidence |
| --- | --- | --- |
| `._name` with no main file is a real file | correct | `a_sidecar_without_a_main_file_is_a_real_file`, `the_sidecar_of_a_sidecar_is_a_real_file` |
| whole-file write that cannot be a sidecar is refused | correct | `a_sidecar_that_is_not_a_sidecar_is_refused_not_dropped`, `a_sidecar_write_past_the_cap_is_refused` |
| the 200+ attribute encoding | correct | `every_count_and_size_of_attributes_survives_a_round_trip` covers `0..=MAX_ATTRS`; the real mount runs 200 `xattr -w` |
| per-inode sidecar locking | correct | `side_write` and `side_setattr` take `PerIno::of(ino)` before reading the buffer; `sidecar_race.rs` runs concurrent writers |
| sidecar handle generations | correct | `a_sidecar_handle_stales_with_its_file`; `Kind::Sidecar` is inside the MAC, so it cannot be flipped on a valid handle |
| MNT answers only one random export path | correct | `only_the_servers_own_export_path_answers_mnt`, `two_servers_never_answer_each_others_mnt`; `secret_path()` is 32 hex digits from `/dev/urandom` |
| one-shot root handle | correct | `only_the_first_mnt_gets_the_root_handle`, `a_gate_off_server_lets_everyone_mount` |
| keyed BLAKE3 handle MAC | correct | `forged_and_guessed_handles_are_refused`, `the_kind_cannot_be_changed_on_a_valid_handle`; random key per server |
| per-connection high-water xids | correct | `reply_cache.rs` `note_xid`/`moved_on`, `a_call_from_another_connection_is_a_new_call_once_the_client_moved_on`, `a_call_the_client_never_got_a_reply_to_replays_on_a_new_connection`, `a_call_still_running_never_gets_a_reply_for_another_connection` |
| oldest-silent eviction at the connection cap | correct | `served_but_silent_connections_do_not_lock_the_client_out`, `slow_and_idle_connections_time_out` |

The dead-server cure is already wired.
`cowfs_daemon::prepare_platform` calls `mounts::sweep_stale` before anything is mounted, and
`cowfs-daemon/src/main.rs` calls `prepare_platform` before `Daemon::start`.
`cowfs_nfs::sweep_stale_mounts` reads `nfsstat -m`, keeps only `localhost:/cowfs-<32 hex>` v3 mounts
under the prefix, skips any whose port still answers, and force-unmounts the rest.
It never walks a path and never unmounts anything it did not find in the table.

`install_signal_cleanup` is deliberately *not* installed by the daemon.
`crates/cowfs-daemon/src/daemon.rs` records why: it exits from its own signal thread with
`process::exit(128 + sig)`, which races the daemon's ordered shutdown and wins it most of the time,
leaving the control socket on disk.
The daemon installs its own handler instead and unmounts every export and the default mount in
order.
`install_backstop_signal_cleanup` exposes the platform backstop for a process that only mounts.

## Unmeasurable here, with the reason

* **A second uid.** #43 itself records that no such account exists on the dev machine. The v1
  security model is single-user and the server ignores AUTH_UNIX caller identity, so there is no
  per-caller check to exercise. `MountOptions::check_peer_uid` is off by default and is documented
  as best effort, because `lsof` cannot see the kernel NFS client's socket.
  Reported UNMEASURABLE, not closed.
* **The export path in the process table.** `Mount::new` runs `mount_nfs localhost:/cowfs-<hex>`, so
  the path is in `argv` for the life of that child. A local process that reads the process table
  during the mount can learn it.
  This is already stated as residual risk in `crates/cowfs-nfs/src/lib.rs`. It is not fixed here:
  `mount_nfs` takes the export in `argv` and there is no other channel for it. Reported as a known
  residual, unchanged.

## Not in this lane

* #43's `Store` mode leaving untracked `._` files after a checkout, and the `nlink` requirements,
  are #19 slot 1's AppleDouble surface and were not touched.
* The NFS conformance re-run and the warm-build budget are #43's later items and belong to the
  g3/g4/g5 and #37 lanes.
* `Adapter::durable`, the metadata RPC barrier and the Core flush belong to the PR #96 worker; the
  shutdown control server to the shutdown worker; `import.rs` and the namespace barrier are off
  limits.
* No foreign umount, no group signal, no `treehouse return`.
  The real-mount test used `mount_nfs_available()` guards and the same `Watchdog` unmount pattern
  the existing mount tests use, and every PID it acted on was one it started itself.

## Reproducing

```sh
export CARGO_TARGET_DIR="$PWD/target/ready43"
cargo test -p cowfs-nfs
cargo test -p cowfs-nfs --test mount -- --ignored --test-threads=1 \
  native_tools_cannot_shadow_a_live_sidecar_name
```
