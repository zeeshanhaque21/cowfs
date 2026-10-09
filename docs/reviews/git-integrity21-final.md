# Independent review: PR #104, issue #21 git index and pack-index integrity

Reviewer: independent critic, ready-wave slot 15, lease `68c9beb4122cda0dad992ca97575045`.
Reviewed head: `6df8b9fcfd6f9d54a6af0c8e424f3851fc6ecc71` (verified exact, lease HEAD matches).
Reviewed PR: #104, `investigate/integrity-21` -> `main`, base `46b0f269d5bef4a2c204c25f5b3015da601d3beb`.
Reviewed artifact: `scripts/verify-git-index-integrity.py` (1177 lines) and `docs/verification/ready-21.md`.

**Verdict: harness source audit PASS. Mounted arm BLOCKED, not disproven. One BLOCK-class defect in the PR body.**
Issue #21 stays **open**. Nothing here reproduces the historical corruption and nothing here closes it.

## Findings, most severe first

### F1 BLOCK: merging PR #104 auto-closes issue #21, contradicting the PR's own text

The PR body says `Closes nothing.` on line 1 and `**This does not close #21.**` on line 21.
The commit message says `This does not close #21; the spike's long soak is still outstanding`.
The verification doc says `issue #21 stays open`.

All four statements are contradicted by what GitHub actually has recorded.
Measured with the GitHub API, not inferred:

| Query | Result |
| --- | --- |
| `closingIssuesReferences(first: 20)` | `[issue #21]` |
| `closingIssuesReferences(userLinkedOnly: true)` | `[]` |
| `pullRequest.issue #21` state | `open` |

`userLinkedOnly` being empty means nobody linked it by hand.
The link is **inferred by GitHub's closing-keyword parser**.
The trigger is body line 21, `**This does not close #21.**`.
GitHub's parser is purely lexical: it looks for a closing keyword and an issue reference on the same line, and it does not understand English negation.
The word `close` sits immediately before `#21`, so the reference parses as closing.

Consequence: merging #104 closes issue #21, and with it the bullet the whole task is about.
This is the exact failure mode the task warned about, and it is live right now.

Fix: rewrite the sentence so no closing keyword shares a line with the reference.
Something like `Issue #21 remains open. The spike's long soak is still outstanding.` satisfies GitHub's parser because no keyword appears on that line.
Re-check with `closingIssuesReferences` after editing, expecting `[]`.
The title's `(#21)` is only a reference and is not the trigger; the title can stay.

### F2 MEDIUM: the documented reproduction commands cannot work as written

`main()` hardcodes the binary directory and refuses to run without it:

```
here = Path(__file__).resolve().parent.parent
binaries = here / "bench" / "out" / "ready-21" / "target" / "release"
for b in ("cowfs-daemon", "cowfs"):
    if not (binaries / b).is_file():
        print(f"missing {binaries / b}; build it first", file=sys.stderr)
        return 2
```

The doc's `What ran` block gives:

```sh
cargo build --release -j4 -p cowfs-cli -p cowfs-daemon -p cowfs-gc   # private target dir
python3 scripts/verify-git-index-integrity.py --ops 42 --cookie-entries 2500
```

There is no `CARGO_TARGET_DIR` anywhere in that block, so cargo writes to `./target/release`, not to the required `bench/out/ready-21/target/release`.
Run verbatim, the script exits 2 with `missing .../cowfs-daemon; build it first`.
The sibling doc `docs/verification/ready-43.md:223` does it correctly with `export CARGO_TARGET_DIR="$PWD/target/ready43"`, so this is an omission in one doc rather than a house convention.

I confirmed the omission: `grep -rn CARGO_TARGET_DIR docs/ scripts/ .github/` finds nothing in `ready-21.md`.

Second, smaller defect in the same block: the docstring says `--ops` `is clamped to the 30..60 band`.
`build_ops()` returns **44** entries, and `OpWindow` slices `build_ops()[:size]`.
So any `--ops` above 44 silently yields 44 operations.
`main()` accepts `--ops 60`, the assert `OPS_MIN <= len(self.ops) <= OPS_MAX` passes, and the run declares 44.
Nothing false is printed, because `declared_window.size` is recorded from `len(win.ops)`, but the doc overstates the usable range.

### F3 MEDIUM: teardown can report a mount as gone when it is not (fail-open mount table)

`mount_line_for()` is a substring test with no exit-code check and no second source:

```
out = subprocess.run(["mount"], capture_output=True).stdout.decode("utf8", "replace")
for line in out.splitlines():
    if f" on {mp} " in line:
        return line
return None
```

Three ways that yields a false absence, each of which downstream code reads as "not mounted":

1. `mount` escapes a space in a mountpoint as `\040`, so a mountpoint containing a space never matches the raw path.
2. If `mount` errors or its output format changes, the result is `None`.
3. There is no tri-state. `None` cannot be distinguished from a genuine absence.

`stop()` then converts that `None` into a positive claim, twice:

```
except ProcessLookupError:
    return {"stopped": True, "why": "already gone"}
```

That branch never consults `mount_line_for`, so a dead pid with a live mount is reported stopped.
And on the normal path:

```
still = mount_line_for(self.mount)
return {"stopped": not still, ...}
```

so a parse failure again produces `stopped: True`.

Blast radius is genuinely bounded, and I want to be precise about why: the harness never calls `umount`, never walks a mount recursively, and never deletes anything under a mount.
`grep -nE 'umount|unmount' scripts/verify-git-index-integrity.py` returns nothing.
It relies entirely on the daemon exiting to drop the mount.
So the failure is a false report, not data loss.
It still matters, because the harness's own docstring says that on macOS 26 every `ls` on an abandoned mount hangs for twenty seconds or more.
A false `stopped: True` is exactly how a leaked mount becomes a machine-wide latency problem for the next agent.

Fix: make the mount table tri-state, `mounted` / `verified-absent` / `UNKNOWN`, and never let `UNKNOWN` satisfy a "gone" claim.
Confirm absence with two independent reads, for example `mount | grep -F` plus a `stat -f %d` on the mountpoint and its parent, and treat unparseable output as `UNKNOWN` rather than absent.

### F4 MEDIUM: no device-identity oracle, so the mount arm could silently become a second native arm

Nothing in the harness asserts that the mount arm's repository is on a different filesystem from the native arm.
`mount_line_for` is consulted only at daemon start and daemon stop.
There is no `os.stat().st_dev` comparison, no `statvfs`, no `df`, and no `mount_line_for(snapshot_dir)` on the arm's own path.

This is the false-PASS shape that two sibling harnesses shipped.
If the private NFS mount silently fell back to the underlying local directory, `git clone` would still succeed, both arms would be APFS, and then `pack_compare`, `history_compare` and `worktree_compare` would all pass trivially, because two APFS runs of the same deterministic seed do agree.
The verdict would read `NOT REPRODUCED` for a run that never touched cowfs at all.

To be fair to the author, the path construction is otherwise correct, and I checked each piece:

- `snapshot_dir = daemon.mount / "seed"`, so the mount arm's repo is `<private mount>/seed/work`.
- `arms["mount"] = clone_arm(snapshot_dir, snapshot_dir / "work", "mount")`, so `.git` and the worktree are both created on the mount by real git.
- Every git operation uses `cwd=repo[label]`, which resolves inside the mount.
- `worktree add ../wt1` resolves to `<private mount>/seed/wt1`, inside the mount, not in `--out` metadata scratch.
- The native arm lives at `attempt/arm-native`, on real APFS, which is the intent of a matched control.

So the placement is right by construction and the concern is an absent assertion, not a wrong path.
Fix: add a gate to `verdict_parts` asserting `os.stat(repo["native"]).st_dev != os.stat(repo["mount"]).st_dev` and that `mount_line_for(snapshot_dir)` is non-`None`, recorded before the window and re-checked after it.

### F5 MEDIUM: the doc overstates what `show-index` detects, and the historical shape slips past it

The doc says:

> `idx check` is `git show-index < every .idx`, which validates the idx magic and its own trailing SHA-1.

Measured on **git 2.56.0**, the exact version the PR claims, in a standalone script with no harness import:

| idx mutation | `show-index` | `verify-pack` | `fsck --full` |
| --- | --- | --- | --- |
| pristine | 0 | 0 | 0 |
| 256 zero bytes at a fixed mid-file offset | 128 | 1 | 27 |
| trailing SHA-1 last byte flipped | **0** | 1 | 1 |
| whole idx zeroed, the historical shape | **0** | 1 | 27 |
| truncated to half | 128 | 1 | 27 |
| 8192 byte zero run injected | 128 | 1 | 27 |

I also ran the same table on Apple's git 2.54.0 and the substantive result is identical.

So `git show-index` returns **0 on a fully zeroed idx**, which is precisely the shape issue #21 describes, and it returns 0 on a flipped trailing checksum byte.
The doc's claim that it validates the magic and the trailing SHA-1 does not hold for that shape.

Consequence for the harness, bounded: the per-operation `idx.check` gate at ops 12, 21, 33 and 41 runs `show-index` only, so it would pass on a zeroed idx.
The overall verdict is still protected.
`semantic_checks` separately gates `idx.verify-pack`, `fsck.full` and `fsck.strict`, and `pack_compare` compares the idx sha256 across arms.
My whole-idx-zeroed case produced `verify-pack` rc 1 and `fsck` rc 27, so `out["pass"]` would have been `False`.
The verdict is sound; the doc's characterisation is not.

Fix: correct that sentence in the doc, and consider making per-op `idx.check` require `verify-pack` as well so the in-window gate matches the post-window gate.

### F6 LOW: the long-zero-run scan is a hypothesis generator and is not evidence of integrity

`pack_zero_report` is written to the log and the summary but is **absent from `verdict_parts`**, so it cannot affect the exit code.
That is the right call, and the code says so honestly in its own docstring: `Structure of long zero runs, not a verdict.`

I corroborated the reported figure independently, using the harness's own `zero_runs` on a seed I generated myself through the harness's own `make_seed`:

```
pack  842 KiB   runs_ge16: 1   runs_ge4096: 0   longest_run: 223
idx   1380 B    runs_ge16: 0   runs_ge4096: 0   longest_run: 0
```

`longest_run: 223` is an exact match for the `longest 223B` figure in the task brief, and it confirms the heuristic reproduces.
It also confirms the number is what it looks like: 223 zero bytes inside an 842 KiB compressed pack is ordinary stream noise, not a corruption signal.
So the PR body's result row `long zero runs in any pack/idx | 0 | 0` should be read as a hypothesis-generator output.
With 0 long runs, a 223 byte longest run, and no structural failure anywhere, this line carries no integrity weight and should not be presented beside `git fsck` results as if it did.
Formal Git structural checks plus matching hashes are what support the bounded claim.

### F7 LOW: ruff on the delivered harness exits 1 with 7 findings

`ruff 0.16.2`, no project ruff config exists, and ruff is not in CI, so these are advisory only.

```
PLW1510 x5  subprocess.run without explicit check argument
BLE001     blind except Exception (line 430)
RUF034     useless if-else condition (line 451)
```

The five `PLW1510` findings are deliberate and correct: the harness reads `subprocess.returncode` itself and must not raise on nonzero.
The `BLE001` is the documented fixture-write guard, and its comment explains why.
`RUF034` is genuine dead logic: `(r if isinstance(r, list) else r)` is just `r`, since `.get` is only reached when `isinstance(r, dict)`.

### F8 LOW: cookie results are recorded but deliberately outside the exit code

`readdir_cookie` and `readdir_cookie_sweep` are computed and stored in the summary.
`readdir_cookie_probe["pass"]` and `cookie_sweep()["any_divergence"]` are excluded from `verdict_parts`, with a comment explaining that slot 1 owns the result.
That is defensible under the dispatch, but a reader who runs the harness and sees exit 0 could reasonably assume the cookie was gated too.
Recommend printing `readdir_cookie.pass` and `any_divergence` in the stdout summary so the exit code's scope is not over-read.

## What I verified as correct

**Scope and provenance.**

- Reviewed head is exact: lease HEAD is `6df8b9fcfd6f9d54a6af0c8e424f3851fc6ecc71`.
- The PR adds 2 files and 1435 insertions, and **no production code**: `git diff --name-only 46b0f26..6df8b9f -- crates/` is empty.
- The two files are `docs/verification/ready-21.md` and `scripts/verify-git-index-integrity.py`.
- Dispatch line 11 assigns slot 3 to `#21 Git-index integrity`, scoped to `Private Git/NFS integrity reproduction and scoped regression; coordinate source changes`, so a reproduction-only PR matches its slot.
- No main overlap. `46b0f26..724f81c` touched only `bench/compare.py`, `bench/test_compare_coverage.py`, `bench/test_gates.py` and `docs/benchmark-coverage.md`. The PR's two files are untouched by main.
- Source identity is correctly labelled as base `46b0f26`, not latest main `724f81c`.
- Build-specific identity is correctly labelled as such: git `2.56.0`, `macOS-26.6.2-arm64-arm-64bit`, python `3.12.2`, daemon sha256 `6273f724…`, cli sha256 `4da06042…`.
- The `preflight` record also captures `git config --list --show-origin`, so a stray global gitconfig that changed behaviour would be auditable rather than invisible.
- I found no overclaiming. The doc explicitly refuses powersync, SIGKILL, performance, the 805 MiB soak and public g6 acceptance, and states that it is not a claim the historical corruption is resolved.
- Raw evidence really is gitignored: `.gitignore:10` is `/bench/out/`, and `git check-ignore` confirms it for `bench/out/ready-21/` and for my own critic files.

**The declared window.**

- The doc's 42-operation table matches `build_ops()[:42]` exactly. I compared all 42 names in order and every one corresponds.
- Every step is one `subprocess.run` with the exit code read from `p.returncode` directly.
- No shell, no pipeline, no `$?`, no `head` in an exit-code path.
- A timeout returns `rc: None`, which is never in any `expect` list, so a hung operation fails rather than passes.
- Per-operation `expect` codes are semantically meaningful and were validated against native, not blanket-allowed: `update-index --refresh` on a dirty tree declares `[0, 1]`, `index.refresh.clean` declares `[0]` strictly, and `stash push -u` is required before `pop` because `stash push` ignores untracked files without it.
- Any rc outside `expect` lands in `failed_ops`, and there is no path by which "any failure on both arms" yields a pass.
- Count gating is real: `window_executed` requires `executed == len(win.ops)` on both arms and `window_no_failed_op` requires `failed_ops` empty.
- Skips occur only when a declared prerequisite failed, and the reason is recorded.
- Because `declared == 42`, requiring `executed == 42` forces `skipped == 0`.
- Report-missing cannot pass. `run_idx_check` returning `[]` produces `rc = None`, which is outside `expect`, and `semantic_checks` gates on `bool(rows) and all(rc == 0)`. I confirmed this empirically by moving the pack directory away and watching the gate fail.

**Determinism, which is what makes the matched comparison meaningful.**

- `GIT_ENV` pins author and committer name, email and **both dates**, and sets `GIT_CONFIG_NOSYSTEM=1`.
- `random.Random(20261004)` seeds the incompressible blobs.
- Consequence: both arms produce identical object ids, which is the only reason "are the packs the same" reduces to a byte comparison against a known-good native answer.
- The seed deliberately mixes incompressible random blobs with one 64 KiB all-zero region, so a zeroed region cannot hide inside plausible compressed output.

**Matched-arm comparisons are non-vacuous.**

- `pack_cmp["pass"]` requires a non-empty intersection, every compared idx equal, and no one-sided extras.
- `wt_cmp["pass"]` requires a non-empty comparison and every path equal.
- `hist_cmp["pass"]` requires head equality **and** identical commit lists, so a commit-count difference between arms fails.
- `wt_cmp` covers exactly the window-rewritten paths that the seed comparison deliberately skips, so a planned edit is never mistaken for corruption and a corruption of an edited file is still caught.

**The store reopen is a real cross-process re-read.**

- Daemon A is SIGTERM'd only after its argv is confirmed to carry this run's store and socket.
- Daemon B runs on the **same store** with a new mount and a new socket.
- `cowfs fsck` runs through the fresh mount, then `git fsck --full`, `show-index` and `verify-pack` run through it too.
- The readback oracle compares against the mount arm's own post-window state, and its pass condition is `not bad and len(hashes) == len(want_map) and bool(want_map)`, so a short read cannot pass.
- The daemon argv is `--backend core`, so this is the Core `fsck` and not the path backend's, which is the distinction the doc draws.

**Signal and cleanup safety, checked against the PR96 critic's findings.**

- There is exactly one `os.kill` in the file: `os.kill(self.pid, signal.SIGTERM)`, gated by `owned(pid)`, which requires **both** this run's store path and this run's socket path to appear in the target's own `ps` command line.
- No `pkill`, no `os.killpg`, no group kill anywhere.
- `start_new_session=True`, so the daemon owns its session and cannot be caught by, or catch, a process-group signal aimed elsewhere.
- **The PR96 bare-PID `Drop` finding does not apply here.** There is no `__del__`, no `atexit` handler and no context manager; teardown is explicit.
- **The PR96 unbounded-`umount` finding does not apply here either.** The harness never unmounts at all; it relies on daemon exit.
- Teardown runs from `finally` in `main()`, so an exception cannot leave a private daemon serving with its mount hanging later `ls` calls.
- `Run.rec` writes, flushes and `os.fsync`s every record, so an interrupted run keeps everything up to the last completed step.
- Each attempt gets a fresh timestamped directory and `make_seed` uses `mkdir` without `exist_ok`, so a repeat run cannot silently reuse or force-reset a previous fixture.
- Socket directories are short, mode 0700, and suffixed with this run's pid, which is the right shape for the macOS path-length limit.
- `shutil.rmtree` appears only on this run's own socket directory and on the cookie probe directories under the attempt dir and the private mount's snapshot. No borrowed cache, no corpus, nothing shared.

**Isolation.**

- `shared_snapshot()` is read-only, using only `ps` and `mount`, and is compared before and after as `untouched`.
- I confirmed the live shared daemon independently: pid 15263, lstart `Sat Oct  3 20:44:29 2026`, command `/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/out/live/bin/cowfs-daemon --store /Users/zeeshanhaque/.cowfs`.
- That lstart matches the value the doc records, so the isolation claim is consistent with what is actually running on this machine.
- The claimed private daemon sha256 `6273f724…` belongs to a different binary from this shared one, which is correct and is what the private store and private socket are for.
- I never signalled, mounted, imported to, or otherwise addressed the shared daemon, its store, its mount or its socket.

**The cookie probe is a real kernel test, and its gate is not differential-only.**

- `_getdents_scan` calls `libc.__getdirentries64` through `ctypes` with a harness-chosen buffer, not a library wrapper, so the buffer size and the unlink timing are both under harness control.
- `pages` is counted directly from the syscall return, which is the load-bearing evidence that the client actually resumed from a cookie rather than draining one libc buffer.
- Every name is unlinked as it is parsed, and the record layout is validated on parse, so a malformed record raises instead of being silently skipped.
- The grid is genuinely 16 cells: 4 buffer sizes by 4 entry counts, with the duplicate 2500 case deduplicated.
- The hardest cell, 512 byte buffer against 2500 entries reaching 500 pages on the mount with 0 remaining, is the right observation to lead with, because it distinguishes "the cookie path ran" from "the answer happened to be zero".
- I verified the gate is not a bare arm-agreement check. `readdir_cookie_probe["pass"]` is `mount.remaining == native.remaining == 0`, so it demands an absolute zero. I proved this by shimming `os.scandir` inside the harness module so both arms left 5 entries behind, and `pass` came back `False` with `native_left=5, mount_left=5`.
- I did not verify the concurrent-unlink ordering claim in general. Resumption under deletion is allowed to be order-dependent, and the probes show no divergence, which is a bounded observation rather than a universal POSIX guarantee. The doc's own framing, reported for slot 1 rather than patched, is the right one.
- Slot 1 owns the vendored cookie and readdir requirements, and this PR correctly reports rather than patches it. `git worktree add` and `remove` run as ops 23 and 24 with an exact relative argv and no stdout path parsing, which is the correct handling of the #97 class of bug.

**All four source citations in the doc resolve.**

- `crates/cowfs-meta/src/tx.rs` takes `let cookie = d.next_cookie; d.next_cookie += 1;` and stores a by-cookie key, so a cookie number is never reused.
- `crates/cowfs-vfs-path/src/cookies.rs:7` states that `a cookie stays valid after` its entry is removed.
- `crates/cowfs-vfs-path/src/tests.rs:97` is `fn readdir_cookies_survive_removal_of_the_entry()`.
- `crates/cowfs-nfs/src/adapter.rs:27` is `const READDIR_PAGE: usize = 512;`, matching the harness's docstring citation.

## My own executed evidence

Everything below ran on APFS only.
No cowfs daemon, no NFS mount, no privileged operation, no signal, and nothing outside my own lease.
My files live only in `bench/out/ready-21-critic/`, which is gitignored, and `git status` is clean.

First I ran the harmless synthetic controls, before touching any filesystem under test, as required.

Then I built negative controls against the **delivered, unmodified harness functions**, imported from the reviewed file rather than reimplemented, because a harness that has only ever printed PASS has not been shown to detect the corruption it exists to detect.
Result: **5 of 5 corruption cases detected, 6 of 6 gate assertions `ok=true`, process exit 0.**

| Control | Result |
| --- | --- |
| pristine baseline must pass | `ok=true`, gates PASS, fsck 0 |
| A, 256 zero bytes at fixed offset 1024 | detected, show-index 128, verify-pack 1, fsck 27 |
| B, trailing SHA-1 last byte flipped | detected, show-index 0, verify-pack 1, fsck 1 |
| C, whole idx zeroed, the historical shape | detected, show-index 0, verify-pack 1, fsck 27 |
| D, idx truncated to half | detected, show-index 128, verify-pack 1, fsck 27 |
| E, 8192 byte zero run injected | detected, and `zero_runs` noticed it at offset 460 |
| restore must pass again | `ok=true`, gates PASS, fsck 0 |
| F, pack directory absent | `ok=true`, cannot pass, fsck 26 |
| G, corrupt a tracked worktree file | `ok=true`, `source_hash.pass=false`, mismatch `blobs/r1.bin`, overall `pass=false` |
| H, cookie scan leaving 5 on both arms | `ok=true`, `pass=false`, so the gate needs an absolute zero |

Two of my own controls failed on the first pass and I fixed them rather than reporting around them.
G initially hashed the expected values *after* corrupting the file, so it compared corruption against itself.
H initially patched `_getdents_scan`, but `readdir_cookie_probe` uses `os.scandir`, so the patch never applied and the assertion was vacuous.
Both were my bugs, not the harness's, and both are fixed in the evidence above.

Other true exits recorded:

| Check | Exit |
| --- | --- |
| `negctl.py`, negative controls | 0 |
| `showindex_probe.py` on git 2.56.0 | 0 |
| `showindex_probe.py` on git 2.54.0 | 0 |
| `ruff check` on my two new files | 0, `All checks passed!` |
| `ruff check scripts/verify-git-index-integrity.py` | 1, 7 findings |

## Work I did not do, and why

**My own live private Core NFS mount arm: BLOCKED. Not run, not disproven.**
This is a resource decision on measured grounds, not a judgment about the harness.
To run the mounted arm the harness requires `bench/out/ready-21/target/release/cowfs-daemon` and `cowfs` to exist, and they do not exist in my lease.
The only copy on this host is in lease 3, which belongs to another agent, and using it would both touch another agent's tree and destroy the build provenance the doc depends on.
Building my own was the remaining option, and the measurements ruled it out at the time I checked:

- load average **16.68 on a 16-core machine**, with 44 concurrent build and compile processes, including a live `cowfs-daemon` at 35 percent CPU;
- no warm target directory in my lease. Lease 3's is 405 MB, so a cold release build of `cowfs-cli`, `cowfs-daemon` and `cowfs-gc` was required;
- my instructions cap a foreground run at 600 s, forbid an unbounded soak, and require the wave's `mac-heavy.lock`, which five other agents are actively contending for.

Launching a cold 405 MB release build into a fully saturated machine would have degraded the other active builders and blown my own cap, so I stopped.
The exact unblock, run inside this lease under the wave lock, is:

```sh
cd /Users/zeeshanhaque/Projects/cowfs/.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/15/cowfs
CARGO_TARGET_DIR="$PWD/bench/out/ready-21/target" \
  cargo build --release -j4 -p cowfs-cli -p cowfs-daemon -p cowfs-gc
python3 scripts/verify-git-index-integrity.py --ops 42 --cookie-entries 2500
```

Note that the `CARGO_TARGET_DIR` prefix is mandatory, for the reason in F2.

**The 16-cell cookie grid: not re-run.** Same measured load reason.
I verified its structure from source, that it is 4 buffers by 4 entry counts deduplicated to 16 real cells, that `pages` is counted from the syscall rather than inferred, and that the gate requires an absolute zero rather than arm agreement.
The per-cell counts in the doc are carried as the author's measurement, not re-measured by me.

**The historical 42/42 counts and the grid numbers are carried, not re-run.** They are the PR author's run.
My review independently validates the harness that produced them and independently measures the native arm and the oracle behaviour, but I did not reproduce the mounted numbers and I am not claiming to have.

## CI, read once at the exact head

| Item | Value |
| --- | --- |
| head checked | `6df8b9fcfd6f9d54a6af0c8e424f3851fc6ecc71` |
| rollup state | `SUCCESS` |
| `check (ubuntu-latest)` | `SUCCESS` |
| `check (macos-latest)` | `SUCCESS` |
| `linux-fuse` | `SUCCESS` |
| mergeable | `MERGEABLE` |
| mergeStateStatus | `UNSTABLE` |

Green CI proves nothing about this deliverable, and the reason is structural.
`ci.yml` runs `cargo fmt`, `cargo clippy`, `cargo test --workspace`, and `python3 -m unittest discover -s bench -v`.
That discovery root is `bench/`, so the unit tests cover the bench harnesses and **never** touch `scripts/`.
`verify-git-index-integrity.py` is not imported, not executed and not linted by any CI job.
The harness also needs a private macOS Core NFS mount and the wave lock, neither of which exists on a GitHub runner, so wiring it into CI is not a one-line change.

## Bottom line

The harness is well built and unusually careful about the failure modes that matter here: real exit codes read directly, a genuinely matched native control, non-vacuous cross-arm comparisons, bounded and time-limited subprocesses, append-and-fsync logging, teardown from `finally`, and signals gated on the target's own argv.
My independent negative controls confirm its oracles do fail when the data is corrupt, which the PR itself never demonstrated.

Four things must be dealt with before merge.
F1 is a blocker on its own: merging today closes issue #21, which is the opposite of the stated intent.
F2 is a documentation defect that makes the run non-reproducible from the doc alone.
F3 and F4 are latent false-report and false-PASS shapes that do not affect this run's conclusion but will bite the next person who uses this harness.
F5 is a doc sentence that should not stand as written, even though the overall verdict is protected by other gates.

On the substance: the current Core backend did not reproduce issue #21 bullet 2 in the author's declared 42-operation window, with a byte-identical pack index on both arms and a clean re-read through a fresh daemon on the same store.
That is a real, bounded, checkable result, and the harness that would catch a recurrence now exists and is repeatable.
It is not a claim that the historical corruption is resolved, and the 805 MiB soak remains outstanding.

**Issue #21 must stay open.** Its bullet 2 is unreproduced and unfixed, and bullet 3 belongs to slot 1.
This review does not close it, and neither should merging PR #104.