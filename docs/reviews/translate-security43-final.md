# Independent review: PR #111, NFS Translate sidecar-view guard (issue #43, round 3)

Reviewed head `c5169e437d141978670684234bd4b5a47650e35d`.
Parent `46b0f269d5bef4a2c204c25f5b3015da601d3beb`.
Reviewer lane: ready-wave slot 13 (IDLE READY13), used only as the parent directory of this
review's own `bench/out/translate43-critic/**` artifacts.

Verdict: the 21-line change is correct, minimal, and does what the PR claims on the exact head,
measured over real TCP NFSv3 on both an in-memory and a real block-store backend.
It is also verified against the combined source of PR #111 and PR #96, not assumed.
Recommend merging #111 as one item of #43, with #43 left OPEN.
This is not a closure of #43; the remaining criteria are listed and were not met here.

## Source identity

Both trees were produced with `git archive` from the primary object store into this reviewer's own
directory, and every tracked blob was verified against the tree entry it came from:

| tree | commit | tracked blobs | missing | hash mismatch | extra files |
| --- | --- | --- | --- | --- | --- |
| new | `c5169e4` | 530 | 0 | 0 | 0 |
| old | `46b0f26` | 529 | 0 | 0 | 0 |

`cargo metadata --locked` on the pristine new archive exits 0 and resolves 132 packages, so
`Cargo.lock` is consistent at this commit and needed no repair.
The review's own test was placed in `crates/cowfs-daemon/tests/`, a crate that already depends on
both `cowfs-core` and, on macOS, `cowfs-nfs`, precisely so that no `Cargo.toml` and no `Cargo.lock`
had to change in any reviewed tree.
No production source was edited anywhere by this review.

Toolchain actually used, not quoted from the PR:
`cargo 1.99.0 (5f94df478 2026-08-27)`, `rustc 1.99.0 (b940084d7 2026-09-28)`, `git 2.56.0`,
macOS on the Apple M3 Max dev machine.

`main` moved during this review, from `03bbec8` to `8255706` by another lane's merge of PR #104.
Neither the reviewed head `c5169e4` nor PR #96's head is in `main` at the time of writing.
Every conclusion here is pinned to exact commit ids and to archives of those ids, not to `main`,
so that movement does not affect any of it.

## What the change is

`crates/cowfs-nfs/src/adapter.rs`, 16 insertions:

* a new `Adapter::not_a_view(dir, name)` that answers `NFS3ERR_ACCES` when `side_of(dir, name)`
  is true, and `Ok(())` otherwise;
* one call to it in each of `mkdir`, `symlink` and `link`, placed after `new_name` and before the
  mutating `Vfs` call.

`crates/cowfs-nfs/src/sidecar.rs`, 5 insertions:
`side_target` answers `NFS3ERR_NOENT` when the main name is `.` or `..`.

Tests and documents: `tests/translate.rs` +153, `tests/mount.rs` +77, `tests/common/mod.rs` +1 (the
`ACCES` status constant), `docs/verification/ready-43.md` +227.

## The independent reproducer

The PR's own tests reuse `cowfs-nfs`'s `tests/common` client.
This review does not, so that a mistake in that client cannot make the defect reproduce or hide.
The reproducer here is a hand-rolled XDR NFSv3 client: it builds the ONC RPC record marks itself,
sends them over a real `TcpStream` to the real listener the in-process server binds on an ephemeral
127.0.0.1 port, and decodes the real replies.
One decoder note, because it matters for anyone else writing such a client: this server's `fattr3`
is 84 bytes, not the 92 of RFC 1813 section 3.3.5, because the vendored `specdata3` is two `u32`
rather than two `uint64`.
XDR opaque and string values are padded to a 4-byte boundary and file handles are 41 bytes, so a
handle occupies 48 bytes on the wire.
The decoder here finds a `fattr3` by searching for the `fsid` this server always writes
(`00 00 00 63 6f 77 66 73`, "cowfs") 44 bytes into the record and validates `ftype`, so a wrong
alignment is a hard failure rather than a plausible wrong number.

The same 22 test functions, byte for byte, were built and run against three trees:

| tree | result |
| --- | --- |
| old `46b0f26` | 19 passed, 3 failed |
| new `c5169e4` | 20 passed, 2 failed |
| merged `f537dee` (#111 + #96) | 20 passed, 2 failed |

17 tests pass on all three trees.
3 pass only on the new and merged trees: the LINK guard, the MKDIR/SYMLINK guard, and the
`._.`/`._..` status.
2 pass only on the old tree: the two tests that assert the legacy corruption is present, which is
the intended before/after signal.

Every test prints its own observation.
The raw transcripts are in `bench/out/translate43-critic/log/{old,new,merged}-run.log`.

## The data loss, reproduced independently on the old parent

Over real TCP NFSv3, with the in-memory reference `Vfs`:

```
BEFORE doc: ftype=1 size=19 nlink=1 fileid=2 mode=644
BEFORE doc direct Vfs ino=2 bytes=74686520717569636b2062726f776e20666f78
TRANSCRIPT link(doc,._doc)=0 create(._doc)=0 write(sidecar,0,4096)=0 commit(sidecar)=0
BLOB len=4096 head=00051607000200004d6163204f532058
DOC handle == ._doc handle: true
RESULT link_st: 0  link_attr: nlink 2 fileid 2  create_st: 0  write_st: 0 written: 4096
        commit_st: 0  after: nlink 2 size 4096 fileid 2  handle_is_doc: true
CONTENT AFTER len=4096 head=00051607000200004d6163204f5320582020202020202020000200000009...
```

Every status is 0 (`NFS3_OK`).
`CREATE` of `._doc` handed back a file handle byte-identical to `doc`'s own handle, so the view and
the file were one inode.
`doc`'s 19 bytes were replaced by the 4096-byte AppleDouble blob, whose first bytes are the format
magic `0x00051607`, the version `0x00020000` and the filler `Mac OS X`.
`nlink` is 2, so `rm ._doc` removes a name and not the data.
The listing is `["doc", "._doc"]`.

The same sequence on the real `cowfs-core` backend, not the reference `Vfs`:

```
CORE direct-vfs-ino=9223373136366403585 direct-nlink=2 direct-size=4096 direct-kind=Some(Regular)
CORE CONTENT AFTER len=4096 head=00051607000200004d6163204f532058...
```

A fresh `Core::open` over the same directory, with no `SIGKILL` and no crash, still shows the
corruption, so it is persisted rather than a buffer artefact:

```
REOPEN before close: nlink=2 size=4096 content=len=4096 head=00051607000200004d6163204f532058
REOPEN after close:  nlink=2 size=4096 content=len=4096 head=00051607000200004d6163204f532058
REOPEN names: ["doc", "._doc"]
```

The same input on the reviewed head, on both backends:

```
TRANSCRIPT link(doc,._doc)=13 create(._doc)=0 write(sidecar,0,4096)=0 commit(sidecar)=0
DOC handle == ._doc handle: false
RESULT link_st: 13  after: nlink 1 size 19 fileid 2  handle_is_doc: false
CONTENT AFTER len=19 head=74686520717569636b2062726f776e20666f78
CORE direct-nlink=1 direct-size=19 direct-kind=None
REOPEN after close: nlink=1 size=19 content=len=19 head=74686520717569636b2062726f776e20666f78
REOPEN names: ["doc"]
```

`LINK` is refused with 13 (`NFS3ERR_ACCES`).
`CREATE` of `._doc` is still answered, with the adapter's own sidecar file id
`18446744073709551615`, which is `u64::MAX` and is therefore never an inode the `Vfs` handed out.
The write and commit still succeed, and they land on `doc`'s extended attributes:
`doc` keeps its 19 bytes, `nlink` stays 1, the listing has no `._` entry, and the real Core shows no
`._doc` object at all.

## The other two shapes, and the dot-name status

On the old parent, each of the three calls was accepted and each then broke the attribute channel
for the rest of that mount:

```
SHADOW mkdir:   accepted=true took-the-name=Some((Directory, 2))   CHANNEL create=21  write=10004
SHADOW symlink: accepted=true took-the-name=Some((Symlink, 1))    CHANNEL create=0   write=22
SHADOW link:    accepted=true took-the-name=Some((Regular, 2))    CHANNEL create=0   write=0
SHADOW link: content=len=4096 head=0005160700020000... nlink=2
```

A directory under the name makes `CREATE` answer 21 (`ISDIR`); a symlink makes the write answer 22
(`INVAL`); and a hard link is worse than either, because nothing is refused at all and the content
is destroyed.
In all three cases `doc`'s extended attributes stay empty.

On the reviewed head all three answer 13, no object takes the name, and the channel answers
`create=0 write=0 commit=0` with `user.k` landing on `doc`, its 19 bytes and `nlink` untouched:

```
SHADOW mkdir: accepted=false took-the-name=None
SHADOW mkdir: xattrs-after=[[117, 115, 101, 114, 46, 107]] content=len=19 head=7468652071756963 nlink=1
```

The status correction is also real and measured:

```
old:  CONTROL dot ._.: root=22 sub=22
new:  CONTROL dot ._.: root=2 sub=2      CONTROL dot ._..: root=2 sub=2
```

`._.` and `._..` remain creatable as real files, and `._..` inside a subdirectory does not touch
that directory's attributes or the mount root.

## Negative controls

These all pass on the old tree, the reviewed head and the merged tree, which is the point: a green
control run is what shows the new guard is narrow.

| control | result, same on all three trees |
| --- | --- |
| `._name` with no main file | `mkdir`, `symlink`, `link` all 0, and the names are listed |
| a stored literal `._doc` first, then `doc` | all three answer 17 (`EXIST`), not 13; the stored bytes read back; `doc`'s attributes stay empty |
| the sidecar of a sidecar, `._._x` | a real file, reads its own bytes back, `._x`'s attributes stay empty |
| `LOOKUP ._a/b` | 22 (`INVAL`) |
| a view handle used as a directory or link source | `mkdir` 20, `readdir` 20, `link` 13, `remove` 20 |
| `RENAME other -> ._doc` | 13, the pre-existing status the new guard matches |
| `Hide` and `Store` modes | all three calls still make a real object, `nlink` 2, no extra `Vfs` call for the guard |
| guard ordering: a file used as the parent | 20 for `mkdir`, `symlink` and `link`, never 13 |
| guard ordering: an already-taken ordinary name | 17, never 13 |
| guard ordering: `.` and `..` | 17, refused by `new_name` before the guard |
| a name over `NAME_MAX` (255) | 63 (`NAMETOOLONG`) |
| a slash inside a name | 22 (`INVAL`) |

The guard also runs strictly before any mutation: on the reviewed head, after a refused `mkdir`
`._doc`, `direct(vfs, "._doc")` is `None` on all three trees.

## Regression and source-consistency checks

Same statuses on all three trees, so nothing else moved:

| case | status |
| --- | --- |
| whole write at offset 0 that can never be a sidecar | 10004 (`NOTSUPP`) |
| a header with entry count 0 | 10004 |
| a 4-byte write | 10004 |
| one byte past the 8 MiB sidecar cap | 27 (`FBIG`) |
| an offset 1 TiB out | 27 |
| an offset that would overflow | 27 |
| a partial write at 0 and at 4000 | both 0, both attributes land |
| `REMOVE` of a view | 0, the attributes go, the content stays |
| a second `REMOVE` of the same view | 2 (`NOENT`) |
| `RMDIR` of a view | 20 (`NOTDIR`) |
| an attribute write through a real view | 0, and 0, 1, 7, 64, 200 and 256 attributes each round-trip byte-identically |

The whole-file refusal paths, the minimum-length check and the status values are therefore
unchanged by this commit, which is what the PR says.

## Residual: the guard's window is still open

The guard reads the namespace with `peek` and then mutates it in a separate `Vfs` call.
There is no namespace lock across that pair, and the existing per-inode and handle-generation locks
do not cover a directory's name space.
This was made deterministic rather than timing-dependent, with a `Vfs` wrapper that creates `doc`
inside that window, and it reproduces on the old tree and on the reviewed head alike:

```
RESIDUAL mkdir status: 0
RESIDUAL ._doc after: Some(Directory)
RESIDUAL channel after the race: create=21
```

So a directory still takes the view name when the main file appears between the guard's read and
the mutation, and the attribute channel is then broken the same way it was before.
This is a remaining race, not a fixed one, and no global fix is attempted or claimed here.

## Reachability, stated precisely

* The server binds an ephemeral port on 127.0.0.1 and answers `MNT` for one export path of the form
  `cowfs-<32 hex>`; a guessed path is refused with `MNT3ERR_NOENT`.
* `one_shot_mount` is honoured: the first `MNT` takes the root handle, a second is refused, and
  `rearm_mount` lets the next one in. The root handle is 41 bytes.
* The real-mount test was run once and passes: `native_tools_cannot_shadow_a_live_sidecar_name`,
  1 passed, 0 failed, in 0.36 s. The mount table held only the shared daemon's mount before and
  after, and the shared daemon was not signalled, restarted or unmounted.
* **That mount test does not show kernel-reachable corruption.** The errno the native tools
  reported is `File exists`, which is the macOS client refusing in its own AppleDouble layer before
  any RPC reaches the adapter. So the mount test proves the end-to-end safety property, and the raw
  TCP tests are what exercise `ACCES`. Whether a macOS kernel would ever have reached the corrupting
  path is not established here and is not claimed.
* The exposure is a correctness and data-integrity boundary at the protocol surface, not an
  unauthenticated remote one. The server is single-user, ignores `AUTH_UNIX` caller identity, listens
  on loopback only, and answers `MNT` for one random path, so this is not an Internet-reachable
  exploit. It is still a real integrity defect for any client that speaks NFSv3 to the socket.

## PR claims checked against this run

| claim | this review |
| --- | --- |
| `tests/translate.rs` grew from 9 to 13 | confirmed by counting `#[test]` in each tree: old 9, new 13 |
| `cargo test -p cowfs-nfs`: 137 passed, 0 failed | confirmed, exit code 0, 137 passed across 14 result lines |
| `cargo fmt -p cowfs-nfs -- --check` clean | confirmed, exit code 0, empty output |
| `cargo clippy -p cowfs-nfs --all-targets -- -D warnings` clean | confirmed, exit code 0 |
| format and lint scope | one crate only, `-p cowfs-nfs`; no workspace-wide claim is made or verified |
| real mount, native tools | confirmed once, as above |
| three neighbouring mount tests | not re-run here, reported as not measured |
| g3 conformance, g4 mounted fsx, g5 xfstests, #37 warm budget | not run here |

## CI at the exact head

One read, no polling, no workflow dispatch and no rerun:

```
commit c5169e437d141978670684234bd4b5a47650e35d
rollup: SUCCESS
  check (ubuntu-latest) | SUCCESS
  check (macos-latest)  | SUCCESS
  linux-fuse           | SUCCESS
```

The PR body lists CI as pending; at the reviewed head all three checks are successful.

## Closing references

`closingIssuesReferences` for PR #111 is empty.
The only keyword line in the body is `Refs #43`, on line 97, and `fixes`, `closes` and `resolves`
do not appear anywhere in it, so GitHub will not close anything on merge.
Issue #43 is still OPEN.
That is the correct outcome here, because #43's other criteria are not met.
Issues #101, #103 and #107 to #110 belong to other lanes and are untouched.

## Overlap with PR #96, measured

PR #96 was at `770b723de04f90a43dcc3e0372f94dbaa8d94801` when this review started and had moved to
`b604aa7e8f662b8084f18bc07133f77b06173b53` by the time it was fetched, so every number below is
against `b604aa7e` and against a branch that is still moving.
It is based on the same `46b0f26`.
It changes 26 files, 4172 insertions and 33 deletions, and it edits `crates/cowfs-nfs/src/adapter.rs`
only, by 115 insertions and 24 deletions. It does not touch `sidecar.rs`.

Method-level overlap in `adapter.rs` is exactly three functions:

| function | PR #96 | PR #111 |
| --- | --- | --- |
| `mkdir` | `self.durable(d.ino)?;` after `handed_out` | `self.not_a_view(d.ino, name)?;` before the mutation |
| `symlink` | `self.durable(d.ino)?;` after `handed_out` | `self.not_a_view(d.ino, name)?;` before the mutation |
| `link` | `self.durable(d.ino)?;` after `handed_out` | `self.not_a_view(d.ino, name)?;` before the mutation |

PR #96 also edits `apply`, `setattr`, `commit`, `create`, `create_exclusive`, `remove`,
`purge_sidecars`, `rmdir` and `rename`; PR #111 touches none of those, so there is no second
overlap and no shared method body beyond the three above.

The merge is clean, and this was measured rather than assumed:

```
git merge-tree --write-tree c5169e4 b604aa7e
f537dee844de4278b6384ddb2c548809db0d7a98
Auto-merging crates/cowfs-nfs/src/adapter.rs
```

No conflicts.
The combined source was then built and run, not just read:

* the 22-test reproducer on the merged tree gives 20 passed and 2 failed, an outcome identical to
  the #111 head on its own: the guard still answers `ACCES`, the content and attributes are still
  preserved, and the residual race is still present and still open;
* `cargo test -p cowfs-nfs` on the merged tree exits 0 with 145 passed, which is this PR's 137 plus
  the 8 tests PR #96 brings.

Ordering in the merged source, in all three functions, is
`not_a_view` then the mutating `Vfs` call then `handed_out` then `durable`.
That ordering is what makes the two changes compatible: a refusal returns before anything was
mutated, so no durability barrier is owed, which is the same rule PR #96 applies to its other
early returns.

One hazard to name for whoever merges these: PR #96 documents `durable_or` as making the barrier
unconditional, so that a failed barrier outranks the caller's own status.
If `not_a_view` is ever wrapped in `durable_or`, or moved after the mutation, a barrier failure
would replace `ACCES` with `NFS3ERR_IO` and the refusal would also pay for a barrier it does not
need. Neither is needed today and neither is done here.

Nothing was merged, cherry-picked or rewritten.
No branch, ref, worktree, lease or lease owner was changed by this review.

## Still OPEN in issue #43

Not addressed and not closed by this PR, and not measured here:

* the matched conformance re-run (the g3 gate), the g4 mounted fsx gate and the g5 xfstests gate;
* the #37 warm-build budget;
* `Store` mode leaving untracked `._` files after a checkout, which is slot 1's AppleDouble surface;
* the second-uid check, which is UNMEASURABLE on this machine and which #43 itself records;
* the export path in `mount_nfs`'s argv, a known residual already stated in `cowfs-nfs/src/lib.rs`;
* the guard's check-then-act window, measured above and reproduced on this head;
* the `Store`-mode leak and the non-regular-file, pathconf and `nlink` requirements owned by the
  #19 and #107 to #110 lanes.

## Conclusion

`c5169e437d141978670684234bd4b5a47650e35d` fixes a real, silent data-loss defect, does so at the
right seam, refuses with the status the adapter already used for the same condition, keeps its own
refusal narrow, and survives the merge with PR #96 with the combined source verified by running it.
It is a good change and should merge.
It closes none of #43's other criteria, and the PR is written so that it will not close the issue.

## Evidence

Raw, gitignored, in this reviewer's own artifact folder under
`.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/13/cowfs/bench/out/translate43-critic/`:

```
tests/translate43_critic.rs   the reproducer, sha256
  4f63d6d312a23eb667e3372fc6e2bd441594a29d6e050d6a92005a73fce598a3
  the same bytes sit in work-old/, work-new/ and work-merged/ under
  crates/cowfs-daemon/tests/
archive/          git archive of c5169e4, verified blob by blob
archive-old/      git archive of 46b0f26, verified blob by blob
work-new/         c5169e4 plus the reproducer
work-old/         46b0f26 plus the reproducer
work-merged/      the #111 + #96 merge result plus the reproducer
merge-probe/      a shared, no-checkout clone used only for the three-way merge
log/old-run.log   19 passed, 3 failed
log/new-run.log   20 passed, 2 failed
log/merged-run.log 20 passed, 2 failed
log/nfs-suite.log, log/nfs-suite2.log, log/clippy.log, log/fmt.log, log/mount-test.log
log/merged-nfs-suite.log  145 passed
build/merge-tree.out  the clean three-way merge and its tree id
```

Nothing here is public and nothing is meant to be committed.
The whole folder is 3.9 GiB, almost all of it three separate Cargo target directories, against the
wave cap of 8 GiB per worker.
Free space on the volume after this review was 418 GiB.