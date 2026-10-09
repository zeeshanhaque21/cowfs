# Independent review: PR #112, a mode (a) slot on the mount is scanned before it is returned (#20)

Reviewed at the exact head, independently, with no production code edited and no lease returned.

| | |
| --- | --- |
| PR | [#112](https://github.com/zeeshanhaque21/cowfs/pull/112) `fix(treehouse): a mode (a) slot on the mount is scanned before it is returned (#20)` |
| Head reviewed | `20d0e645b8bf2836fc88978a07e12d0f8d8dd20c` (tree `e655bb13b38e666fc35634facd46ae02699e5f1e`) |
| Merge base | `46b0f269d5bef4a2c204c25f5b3015da601d3beb`, verified with `git merge-base` |
| PR base ref | `03bbec85626c26a85ee4f5791d3a413fe47725bc` (main moved on; remote main is now `951045fca4823611e196eda75db0c977a46d2c77`) |
| Diff | 13 files, +1376 / -72, 6 commits |
| CI | run `37258217883`, head SHA matches, 3 jobs green, verified job by job |
| Closing refs | exactly 1, `#20` itself. No other issue is auto-closed |
| Verdict | **BLOCK** on one fail-open defect. Merge is not blocked by anything else in this review |

Evidence lives in `bench/out/holders20-critic/**` inside the reviewer's own lease, with a
`sha256` manifest of every file under `src/*/.sha256-manifest` proving which tree was built.

## What was actually verified here, and how

The source was taken from the exact head over authenticated HTTPS with `git archive`, never a
checkout: four trees (`head`, `base46`, `base_pr`, `main`), each verified byte-for-byte against
`git ls-tree -r` blob ids before any `cargo` ran. `probe/verify-manifest.js` reports 0 mismatches
over 531 tracked blobs for the head.

| check | result |
| --- | --- |
| Old fail, new pass, same public caller fixture | base `46b0f26` **0 passed, 4 failed, exit 101**; head **4 passed, exit 0**. Fixture sha256 `a472546b93eb20ef7650d075522f364616cb470a7a3db0a9b0431e711836ff43` compiled into both trees |
| `cargo test --locked -p cowfs-treehouse -p cowfs-ctl -p cowfs-daemon -j2` at head | **270 passed, 0 failed, 5 ignored, exit 0**. The PR body's numbers are exact |
| `cargo fmt --all --check` at head | exit 0 |
| `cargo clippy --locked` three crates, `--all-targets -j2 -D warnings` | exit 0 |
| The 5 ignored | all in `cowfs-nfs/tests/bench.rs`, a crate this PR does not touch. Pre-existing, not laundered |
| CI run `37258217883` | `check (ubuntu-latest)`, `check (macos-latest)`, `linux-fuse`, all SUCCESS, head SHA matches. Ubuntu ran the four new tests for real, not skipped: `test result: ok. 4 passed` |
| Live private Core NFS mount, private pool inside it, real holder | reproduced end to end, exit codes and digests below |
| Integration against `main` | `git merge-tree --write-tree` clean against both `03bbec8` and `951045f`, and no drift at all in the six shared files between `46b0f26` and `03bbec8` |

Nothing was borrowed. Three private daemon attempts were run inside this reviewer's own fixture, each
with its own store, mount, socket, `HOME` and pool, and each torn down by an identity check on pid,
argv and start time before any signal. The shared daemon `15263`, its `/Users/zeeshanhaque/.cowfs`
store and mount, and all 16 treehouse leases were left alone and re-verified afterwards: 16 of 16
still leased, the two pre-existing NFS mounts unchanged.

## The live deliverable, reproduced independently

A private daemon on the real Core backend, a real macOS NFS loopback mount, a real
`treehouse get --lease` slot **inside** that mount, and a real holder process that had `chdir`'d to
`/` with only an open descriptor to claim the slot:

- The real adapter's `ps`, asked with the mount-relative slot directory, named the holder with an
  `fd` hold and **no** `cwd` hold anywhere in the output. That is the discriminating property: a
  detector that only read working directories would have returned nothing.
- `cowfs-treehouse return` with no `--force` exited **5**, naming both pids, the held file and
  `--force`. The treehouse `lease_id` was identical before and after
  (`09684b6df6aaae9effe35d9147b10461`), the file was still there, the holder was still alive, and
  nothing had been signalled.
- The unlink a reset performs left `.nfs.200516fc.4110` in the slot, exit 0, sha256 identical to the
  source (`991e293887170e8534a47638a8e6549ff8eddada7b28566e0a0187757fee90f9`, 34 bytes).
- Reading through the holder's real descriptor **after the name was gone** gave the same sha256 and
  the same 34 bytes. The data is intact; only the name moved.
- A second `return` with the dirt present exited **5** again and named the silly-rename itself.
- Native baseline, same held unlink on an ordinary directory: exit 0, entries `. ..`, zero
  silly-renames.
- After a verified-identity `SIGKILL` of the holder, the silly-rename was gone within 2s **with the
  mount still up**. That answers issue #20's open question: the `.nfs` file appears at unlink time
  and disappears at descriptor-release time, not at unmount time.
- `ps ../escape` was refused with `invalid_params`, and the refusal message quoted the rule.

Two containment claims in the body also hold by construction and by probe: `Path::starts_with` and
`strip_prefix` are component-wise, so `/mnt/slot-other` cannot pass for `/mnt/slot`, and the daemon
compares a `canonicalize`d path against a `canonicalize`d `mount_path`, so a symlink escaping the
mount is refused.

Two more negative cases the PR does not test, both run here against the real daemon:

- `--force` where a holder is the invoking shell's own pid: exit **5**, message
  `is held by pid ..., and this process may not signal them; the slot was left in place rather than
  reset under a live holder`, and the shell was still alive afterwards. The ancestry guard is real.
- An unanswerable scan reaches the companion as `Unsupported` and therefore exits **1**, not 5.
  That is deliberate: a blind scan is not a busy slot.

## Findings

### 1. BLOCK: an `lsof` failure still reads back as "the slot is clean"

`crates/cowfs-daemon/src/holders.rs:327` maps lsof's exit 1 to `Ok(text)`, on the stated reasoning
that lsof exits 1 "when it matched nothing, which is an answer". That is only true when lsof exits 1
*after a complete walk*. It is not the only way lsof exits 1, and the difference is not observable:
in both cases stdout and stderr are empty, so nothing downstream can tell them apart.

Measured on this machine, lsof 4.91, with a real holder and a real private NFS mount:

```
holder pid 87660 holds fd 9 -> <slot>/locked/held.txt, cwd is /
CONTROL  ps (everything readable)      -> 2 holders, both kind "fd"
         chmod 000 <slot>/locked        (holder unaffected, fd still open)
         lsof -nP -w -F pcfn +D <slot>  -> rc=1  stdout 0 bytes  stderr 0 bytes
         ps <slot>                       -> {"processes":[]}      <-- CLEAN
         cowfs-treehouse return          -> exit 1, and it proceeded to `treehouse return`,
                                            which is the reset the whole PR exists to prevent
```

Any subdirectory inside the slot that the scanning user cannot read produces this. `+D` is a
recursive walk; when it hits a directory it cannot read, lsof gives up and exits 1 with no
diagnostic. `parse_lsof("")` returns `[]`, so `scan_checked` returns `Scan::Holders(vec![])` and the
daemon answers `Ok`, not `Unsupported`. This is precisely the fail-open the PR set out to remove,
still reachable through the one exit code it decided to trust.

It is not exotic. Slot contents include whatever a build wrote, and a `chmod 000` directory in a
worktree is ordinary. It also needs no privilege: the restrictive directory can belong to another
account on a shared pool.

The fix is small and belongs where the ambiguity is: treat exit 1 as an answer only when the walk is
known to have completed, or treat an empty parse of a non-zero-exit lsof as
`Scan::Unavailable`. The blunt version, `Some(0) | Some(1)` becoming `Some(0)` plus an explicit
"matched nothing" signal, would be safer still, at the cost of refusing clean slots on some lsof
builds.

Note the same probe also shows the second half of the claim failing for a benign reason: with the
slot clean, the return did not exit 5, it exited 1 from `treehouse` itself, with
`git clean -fd: warning: could not open directory 'locked/'`. So the slot would still have been
reset, just with a noisier message.

### 2. The Linux `/proc/locks` parser reads the wrong field and the wrong radix

`crates/cowfs-daemon/src/holders.rs:108-122` parses each `/proc/locks` line as
`(_kind, _pid, dev, ino)` from the first four whitespace fields. The kernel's line is
`id: CLASS MODE ACCESS pid MAJ:MIN:INODE START END`, so the device field is at **index 5**, and the
head parser reads `"ADVISORY"`. Compounding it, `maj.parse()` is decimal while `locks_show` writes
`%02x:%02x`, so the major and minor are hex.

Run on a real aarch64 Linux (`moonscape`, kernel 6.12) against a real `flock(1)` on a real file:

```
REAL /proc/locks row: "27: FLOCK  ADVISORY  WRITE 1327043 00:40:228185 0 EOF"
  tokens = ["27:", "FLOCK", "ADVISORY", "WRITE", "1327043", "00:40:228185", "0", "EOF"]
  head parser reads index 2 as the device field: Some("ADVISORY")
  the device field is actually at index 5:      Some("00:40:228185")
locked_exact_head()  produced 0 triples
locked_corrected()   produced 109 triples
st_dev (decimal, what libc::major/minor return) = (0, 64, 228185)
head set contains it?      false
corrected set contains it? true
```

The head parser produces an **empty** set, so `HoldKind::Lock` is never reported on Linux at all.
The `locked()` refactor in `c82bb35` made an always-empty function fail-closed about being
unreadable, which is an improvement, but the failure it now guards against is not the one that
matters: the table is being read successfully and misparsed.

This is pre-existing, not introduced here: `git diff 46b0f26..head` on that function is only the
`Result` wrapper and the error message. But the PR's body and `ready-20.md` both present the Linux
`fd`/`lock` scan as working, and its own CI runs `holders::tests::a_holder_that_chdird_out_...` on
ubuntu, where the fd half of that test does pass and the lock half is not exercised by any test.
So the PR inherits a Linux lock scan that reports no locks, and says nothing about it.

Fix: index 5, and `from_str_radix(_, 16)` for major and minor. Then add a Linux test that takes a
real `flock` and asserts `HoldKind::Lock`, which nothing does today.

### 3. `PoolEntry::leased()` is dead code, so the `status`-string fix changes nothing yet

The body calls this out as a fix: `leased` deserialised as a bool and so was always false. That part
is right, and `impl PoolEntry { pub fn leased(&self) -> bool { self.status == "leased" } }` is a
correct reading of treehouse's `status` column. But `rg '\.leased\(\)'` over the whole head tree
returns **nothing**. The old `leased: bool` field was equally unused. So this changes a field no code
reads, and the release-safety property it sounds like it protects is not yet wired to anything.

Two follow-ups belong with it rather than in review: nothing guards on `status` today, so an unknown
or renamed status string is silently "not leased"; and there is no test for `status` to bool, while
the two other test files assert `status == "leased"` on raw JSON instead. Either use the accessor
where the release decision is made, or drop it and keep the field.

### 4. Minor: `ps .` scans the entire mount

`validate_mount_relative` accepts `.` and `a/./b`, and the daemon's containment check is
`canonical.starts_with(mount_path)`, which is true for the mount root. So `ps .` is a legal request
that walks the whole mount with `lsof +D`, which is the exact operation `SCAN_TIMEOUT` exists
because it can wedge. Measured on the private mount, `ps .` and `ps <slot>` both returned the same
4 processes here, in 1s and 0s, so on a small mount it is harmless; on a large one it is a scan of
the wrong size with no bound beyond the 10s timeout, after which it degrades to `Unavailable` and
blocks every return. Worth refusing the bare `.` while keeping `a/./b`.

### 5. Unbounded canonicalize on the `ps` path, against the repo's own recorded trap

The new guard at `handler.rs:290` runs `std::fs::canonicalize(&dir)` with **no** timeout, before
`scan_checked`'s own 10s bound. This repo has already been bitten twice by exactly this:
`90c9a8f` "a stale NFS mount must not able to wedge teardown", where `os.path.realpath` on a dead
NFS mount blocked indefinitely and made even `ls` hang, and `265fc3f` "an unreadable mount entry
blocks cleanup". The fix there was to stop resolving paths in the risky place.

I could not measure this one: producing a stale NFS mount here needs privileges this lane does not
have, and `mount_nfs` against a dead server refuses outright (`rc=61`), so the probe reports
UNREACHABLE rather than guessing. The pre-existing code had the same exposure inside
`holders::scan`, so this is not a regression, but it is a new call on the RPC path and the repo
already knows the shape of the failure.

### 6. Already disclosed, no action needed

`exports.rs` still calls the best-effort `holders::scan`, so `mount_snapshot` and `unmount_snapshot`
keep today's fail-open behaviour and now log the reason. The body says so plainly. Correct to leave
alone; it should stay on the list.

## What the PR claims, checked against what is true

| claim | verdict |
| --- | --- |
| Old fail / new pass, 0 and 4, exit 101, then 4 passed | **true**, reproduced with the same fixture |
| 270 passed, 0 failed, 5 ignored | **true**, exact |
| clippy exit 0 | **true** |
| `ps` names fd holders with no cwd hold in a real mode (a) slot | **true**, reproduced on a private NFS mount |
| `return` exits 5, names pids, file and `--force`, lease intact | **true**, reproduced, lease id identical |
| the unlink becomes `.nfs.<id>` with identical content | **true**, reproduced, digest matched |
| descriptor read after the name is gone has the source sha256 | **true**, reproduced |
| the silly-rename disappears at release with the mount up | **true**, reproduced |
| scan is fail-closed: missing lsof, unexpected rc, deadline, unreadable `/proc` all `unsupported` | **partly true**. Missing lsof, deadline and `/proc` read errors are. An lsof exit 1 is trusted unconditionally and still reads clean, see finding 1 |
| `lsof` pipes drained off the waiting thread so the deadline is reachable | **true**, and a real improvement. But `out_rx.recv_timeout(5s).unwrap_or_default()` discards output that has not arrived, so a grandchild holding the pipe turns a real answer into `Ok("")` and therefore a clean slot. Reproduced with a line-for-line replica: `text_len=0`, `parse_lsof` sees `[]`, reads clean |
| a pid this process may not signal is refused | **true**, exit 5, reproduced with the invoking shell as the holder |
| `PoolEntry.leased` was always false | **true** and now unread by anything, see finding 3 |
| FUSE not measured | **true and honestly stated** in `ready-20.md`. Linux is stated as measured; on Linux the `lock` half reports nothing, see finding 2 |
| upstream proposal is a draft, not sent | **true**, `docs/upstream-treehouse-proposal.md` section 3, no send recorded |

On the closing-reference risk: the body says `Closes #20.`, `closingIssuesReferences` is exactly one
entry, `#20`, and no commit subject or body carries a closing keyword for any other issue. There is
no risk of an incomplete acceptance criterion being auto-closed here. `#20` itself stays open after
merge, which is right: its FUSE item is unmeasured, its `.nfs*` handling item is undecided, and its
upstream proposal is unsent.

## Recommendation

Merge after finding 1 is fixed and finding 2 is either fixed or explicitly downgraded in the body
with the measurement above attached. Findings 3, 4 and 5 are follow-ups, not merge blockers. Finding
2 in particular should not be described as "measured on Linux" in the body while the lock half
silently reports nothing.

Issue #20's own checklist is only partly discharged by this PR, and closing it is not what this PR
should claim: the upstream proposal is unsent, the `.nfs*` readdir / `git status` decision is
untouched, and the FUSE re-check is unmeasured.

## Reviewer notes on process

- The g5 critic's branch, reports and fixtures in this lease were treated as read-only. HEAD stayed
  at `6a8075aedd53a69d6f6735acace5b6817f771290` on `review/xfstests-g5`; no checkout, reset, stash
  or lease return.
- `docs/reviews/xfstests-g5-final-repair-review.md` appeared in this worktree while I worked. It is
  not mine and was left alone, along with every other untracked file.
- The primary checkout has unrelated modified and untracked files, including
  `docs/upstream-treehouse-proposal.md` and `docs/v1-treehouse.md`, which this PR also changes. Those
  are user-owned working state; nothing was cleaned up or reverted.
- One honest correction to my own first run: my live holder used `exec sleep`, so the `SIGUSR1`
  read-back killed it instead of reading, and three assertions failed for that reason alone. The
  author's holder kept its shell alive in a loop, which is why theirs worked. After fixing the
  holder, every assertion passed. Recorded here because the first result looked like three product
  defects and was one harness defect.
- Owned report path: `/Users/zeeshanhaque/Projects/cowfs/docs/reviews/open-descriptor20-final.md`.
  Raw evidence: `bench/out/holders20-critic/` in this lease. No production file was modified, no
  commit, no push, no merge.