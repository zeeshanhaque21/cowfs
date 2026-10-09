# g5 xfstests gate, independent review at 6a8075a

Reviewer: independent critic, lease `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/12/cowfs`, branch `review/xfstests-g5`.

Reviewed head: `6a8075aedd53a69d6f6735acace5b6817f771290` (verified with `git rev-parse HEAD`, tree clean, no tracked edits made by this review).

PR: #100, head `6a8075aedd53a69d6f6735acace5b6817f771290`, state OPEN, no auto-merge request.

Author of the change under review: ready-wave slot 11, branch `verify/xfstests-g5`.

## Verdict

**SOURCE and safety review: BLOCK. Do not merge.**

**g5 acceptance: still UNMEASURABLE, still OPEN. Zero measured assertions, same as the builder reported. Issue #101 remains open and is still the correct authority for the block.**

Two independent things are true and must not be conflated.

1. The builder's own headline is honest.
   No generic case ran on either arm, the reason is the suite's own prerequisite gate, and both the PR and the verification document say UNMEASURABLE and keep g5 open.
   I reproduced the refusal path and it is genuinely fail closed.
2. The harness that is supposed to produce the future pass is not yet safe to hand to a future operator.
   Its two arms do not run on the two filesystems it is told to compare, and it can emit `VERDICT: PASS` while doing so.
   That is a false-pass mechanism in the exact code path the PR exists to add.

The block is about the harness, not about cowfs.
No cowfs defect was found, and nothing here says cowfs is or is not xfstests conformant.

## What actually ran, with real exit codes

Captured from the child process directly, never from a pipeline.
All of it ran on this Mac inside my own scratch directories.

| step | what it was | exit | result |
|---|---|---|---|
| PR100 unit suite | `python3 -m unittest discover -s bench` | 0 | 73 tests, OK, log in `bench/out/ready-g5-critic/unittest.log` |
| PR100 lint | `ruff check --isolated --select F,E9 bench/xfstests_gate.py bench/test_xfstests_gate.py` | 1 | 1 error, `bench/test_xfstests_gate.py:12` F401 `os` unused |
| control A | real CLI `preflight` on a synthetic tree whose required helper is absent | 2 | UNMEASURABLE, 4 blocking entries, correct reason, probe rc 1 |
| control B | `gate.run` with two separate arm roots, child echoes `$TEST_DIR` | 0 | **printed `VERDICT: PASS`, and both arm roots stayed empty** |
| control C | `gate.verdict_for_case` truth table | n/a | native rc 1 plus cowfs skip signature scores PASS |
| control D | recorded capability claim vs this host | n/a | claim false here |
| control E | `classify` with a drifted allowlist | 0 | drift printed, exit 0 |
| control F | `report` on a FAIL record | 0 | FAIL printed, exit 0 |
| control G | `--require-full` denominator | n/a | suffixed entries silently excluded |

Controls A through G are all in `bench/out/ready-g5-critic/arm_probe.py`, which is ignored and local.

**Controls B through G are harness-level controls, not xfstests results and not acceptance evidence.**
They use a synthetic tree and, where the startup gate had to be satisfied, the same in-process gate narrowing the repository's own unit tests already use.
They prove what the harness code does.
They prove nothing about any filesystem.

## Findings

### BLOCKER 1. The two arms never run on the two roots, and the gate still says PASS

`--native-root` and `--cowfs-root` are parsed, checked with `is_dir()`, recorded into the run's meta record, and then never used again.

`run()` at `bench/xfstests_gate.py:562-578` passes `run_dir / cid / "native"` and `run_dir / cid / "cowfs"` to `run_case`.
`run_case` derives the test directory from that work directory, at line 470, and `arm_env` sets `TEST_DIR` and `TEST_DEV` to it at line 477.

So `TEST_DIR` is always `<--out>/run-<ts>/<cid>/<arm>/testdir`.
Both arms are two directories under `--out`, on the same filesystem, created by the harness.

Control B measured this from the child's own output rather than from reading the code:

```
native_root passed in: .../b/ARM-NATIVE
cowfs_root  passed in: .../b/ARM-COWFS
--out passed in:       .../b/out
native  child says TESTDIR=[.../b/out/run-20261004-165730/005/native/testdir]
cowfs   child says TESTDIR=[.../b/out/run-20261004-165730/005/cowfs/testdir]
native TESTDIR under ARM-NATIVE?  False
cowfs  TESTDIR under ARM-COWFS?   False
both TESTDIRs under --out?         True
native files in ARM-NATIVE after run: 0
cowfs  files in ARM-COWFS  after run: 0
```

The gate answered `0` and printed `VERDICT: PASS` on that run.

Why this matters more than a plain bug: `docs/verification/ready-g5.md:182` tells the next operator to run

```
xfstests_gate.py run --xfstests DIR --native-root DIR --cowfs-root DIR --cases 005,236,245,309,360,755
```

with the native root a private ext4 directory and the cowfs root a snapshot inside the private mount.
Following that instruction exactly changes nothing about where the cases run.
The operator would get an exit 0 PASS from a run that never touched either root, and the meta record would cheerfully name both roots as if it had.

The repo's own unit test cannot catch this, because it distinguishes the arms by a substring of the path:
`bench/test_xfstests_gate.py:354` uses `case "$TEST_DIR" in *cowfs*)`.
That matches the literal directory name `cowfs` in the harness's own layout, so the test passes while the arms are the same filesystem.

Required before merge: place each arm's per-case directory inside its own root, assert that containment in code, and add a test that fails when the arm root is not on a different device or mount from the other arm.

### BLOCKER 2. A case that asserts nothing and exits 0 scores PASS

`verdict_for_case` has no assertion count, no output-length floor, and no comparison of what the two arms produced.
Its only defence is `SKIP_SIGNATURES`, six literal regexes.

Control C, third line:

```
both rc=0, EMPTY logs, no signatures  ->  PASS | both arms exited 0 with no skip signature
```

The builder's document at `docs/verification/ready-g5.md:93` describes hitting exactly this class of failure, a probe that exited 0 having asserted nothing, and at line 112 claims the per-case log check is what catches it.
The log check catches the cases the six regexes happen to name.
A case that prints nothing and exits 0 is not one of them, and it is scored PASS.

This is the precise hazard the gate was written to prevent, so it needs a positive check rather than a blacklist: record a non-empty log, and treat an empty or assertion-free log as INVALID, never PASS.

### MAJOR 3. A native-arm failure escalates into a counted pass, and the cowfs skips are never read

`verdict_for_case` returns from its first branch, `native["rc"] != 0`, before it ever looks at `cowfs["skips"]`.

Control C, first two lines:

```
native rc=1 (skip seen) + cowfs rc=0 (skip seen) -> PASS | native failed and cowfs passed; recorded as a native-arm failure
native rc=1 no skips    + cowfs rc=0 (skip seen) -> PASS
```

The verdict mapping itself is intentional and locked in by `test_native_failure_is_never_a_cowfs_verdict` at `bench/test_xfstests_gate.py:335`.
Treating a native-arm failure as not-a-cowfs-verdict is defensible.
What is not defensible is that the result increments `tallies[VERDICT_OK]`, and that a run in which native is broken and cowfs silently no-ops exits 0 PASS.
No test covers the tally or exit-code consequence of that mapping.

Required: give the native-failed case its own tally bucket, and refuse PASS when any arm's log carries a skip signature regardless of which arm failed.

### MAJOR 4. Nothing binds the executed source to the reviewed allowlist

The allowlist is a text file of bare numeric ids.
The drift check at `bench/xfstests_gate.py:341-350` compares the committed id list to the ids the classifier scores SAFE on the live tree.
That is a set-membership check, not a content pin.

Consequences, all verified:

- `allowlist_sha` is `sha256` of the newline-joined id list, via `sha_of` at line 609.
  It identifies the list, not a single byte of any test source.
- The pinned tree sha `3e1ee800...` appears nowhere in the harness.
  It is only a comment in the allowlist header, and `read_committed_allowlist` at line 329 parses bare numeric lines only, so the comment is never read.
- `pre["source"]["sha"]` is recorded into meta and never compared to anything.
  There is no `source[` comparison in the file.
- `git_provenance` records `dirty` and `dirty_paths` at lines 233-236 and never acts on them.
  A modified xfstests checkout runs.

So the answer to the question that matters for safety is no.
A modified allowlisted case that keeps its lexical verdict runs with no signal, and the run is filed under the same `allowlist_sha` as the reviewed one.
The classifier is a regex pass over `tests/generic/NNN` only.
The builder's own document records that this classifier had three passes and wrongly scored 005, 123 and 184 as safe until their sources were read, at `docs/verification/ready-g5.md:114-120`.
A lexical classifier is a hypothesis about a source, not proof that the source is safe.

Required before any run is trusted: pin the tree sha in the allowlist as machine-readable data, compare it, refuse a dirty tree, and record a per-case source hash next to every executed case.

### MAJOR 5. Transitive `common/*` is allowed by name and never read or hashed

`ALLOWED_SOURCES` at line 148 whitelists `rc`, `preamble`, `filter`, `list`, `config`, `promotion`, `util`, `attr`, `pwrite-buffers`, `rc.local` and `ftruncate.inc`.
`SOURCE_RE` at line 152 records only that a case sources one of them.
The contents of those files are never read, never hashed, and never reviewed.

`common/rc` and `common/util` are executable code that runs inside every allowed case and can reach anything on the host.
They are trusted by filename alone.

Required: hash and pin the `common/*` files an allowlisted case actually pulls in, or refuse the case.

### MAJOR 6. `capabilities_absent` is a hardcoded literal and is false on this host

`preflight` assigns a fixed dict at `bench/xfstests_gate.py:406-411`.
It is never probed.

Control D:

```
actually on PATH: autoconf -> None
actually on PATH: automake -> None
actually on PATH: libtool  -> /usr/bin/libtool
actually on PATH: m4       -> /usr/bin/m4
gate records: 'autoconf/automake/libtool/m4 absent' -- hardcoded at xfstests_gate.py:409
```

The preflight record is the evidence artifact a future reader trusts, and it asserts host capabilities that were never checked.
On this Mac two of the four are wrong.
`block_scratch_device` and `getfattr/setfattr` are equally unprobed.

Required: probe each capability, or record only what was actually measured.

### MAJOR 7. `classify` exits 0 on drift, contradicting both the document and the PR body

Control E:

```
DRIFT: allowlist drift: added ['005'], removed ['999']
classify rc = 0
```

`classify_cmd` prints the drift and returns 0 at line 651.
`run` does check drift and returns 3, so a `run` is protected.
A CI step or an operator running `classify` alone gets a green exit on a drifted allowlist.

`docs/verification/ready-g5.md:109` says "classify recomputes the set and exits 3 on drift".
PR #100 line 15 repeats it.
Both statements are false against the code in this same commit.

### MINOR 8. A missing probe case makes `preflight` exit 1, which collides with FAIL, and writes no evidence

`preflight` hardcodes `tests/generic/010` as its probe at line 414.
If that file is absent, `run_case` raises `FileNotFoundError` from `Popen`, nothing catches it, and the process exits 1 with a traceback.

Measured:

```
RETURNCODE: 1
FileNotFoundError: [Errno 2] No such file or directory: './tests/generic/010'
evidence file written: False
```

The documented exit set is 0 PASS, 1 FAIL, 2 UNMEASURABLE, 3 INVALID, and line 22 of the module docstring asserts a missing prerequisite prints UNMEASURABLE and exits 2.
Here it exits 1, which a consumer reads as FAIL, and `preflight.jsonl` is never written, so the failure leaves no evidence at all.

### MINOR 9. `report` exits 0 on a FAIL run

Control F: `report rc = 0` on a record whose verdict is FAIL.
Report is a printer, but a CI step wired to it goes green on a failing run.

### MINOR 10. `--require-full` is coverage of whatever tree is pointed at

`total = len(records)` at line 589, and `classify_group` counts only regular files matching `^[0-9]+$` directly under `tests/generic`.

Control G:

```
entries under tests/generic: ['005', '069_o_tmpfile', '236', '307_recovery', '317', '504']
classify_group records: ['005', '236', '317', '504'] = 4
EXCLUDED from the denominator: ['069_o_tmpfile', '307_recovery']
```

Real xfstests `tests/generic` contains suffixed entries and subdirectories in the `307_recovery` style, so they are silently outside the denominator.
No case count and no tree sha is pinned anywhere.
`docs/verification/ready-g5.md:187` and PR #100 line 54 both describe the flag as the thing that refuses to call g5 done until all 802 cases ran.
It cannot distinguish 802 from any other count, and it cannot tell you the tree is the reviewed tree.

On the 802: the number is the count of bare-numeric regular files in `tests/generic` at tree `3e1ee800`.
It is version dependent.
I could not independently re-derive it, because there is no xfstests tree on this Mac and no `COWFS_XFSTESTS_SRC` set, so 802 and the per-verdict breakdown in the verification document are builder-reported and I am not restating them as measured.
The one number I did reproduce is the allowlist sha, from the six ids alone: `sha256("005\n236\n245\n309\n360\n755")` equals the `5eab2e8a473b26613e279266bfb1812426e2a8bfbd256aef2eae9920cb2c6671` in the provenance table.

### MINOR 11. Lint fails

`bench/test_xfstests_gate.py:12` imports `os` and never uses it.
`ruff check --isolated --select F,E9` on the two files exits 1.

### MINOR 12. The timeout kill is a process-group signal

`bench/xfstests_gate.py:493` calls `os.killpg(proc.pid, 9)`.
`docs/ready-wave-dispatch.md:85` and the repository `AGENTS.md` both say no process-group signals, without exception.

The blast radius is genuinely correct here, because line 486 sets `start_new_session=True`, so the child's process group contains only that case and its own descendants, and killing it is necessary to stop a timed-out case writing into the next case's directory.
I am not calling this a live hazard.
I am flagging that it contradicts a categorical standing rule and needs the lead's explicit sign-off rather than passing silently.

Two smaller things in the same block: `rc` is overwritten with a literal `-9` at line 495, discarding the real wait status, and `os.killpg` is unguarded, so a child that exits between the timeout and the signal raises `ProcessLookupError` out of `run_case` and aborts the whole run.

### MINOR 13. `preflight.jsonl` is append-only into a fixed name

`write_jsonl` opens in append mode at line 433, and `preflight` always writes `out_dir/"preflight.jsonl"`, so repeated preflights accumulate records in one file with no run identifier.
The builder worked around this by renaming to `preflight-run1.jsonl`, which is visible in the evidence table at `docs/verification/ready-g5.md:165`.

## Safety properties that do hold

These I checked and they are right.
Recording them so the fix is not larger than it needs to be.

- **Case id traversal is blocked.**
  `read_committed_allowlist` accepts only `^[0-9]+$` lines, and `classify_group` derives ids from `path.name` of files matching `^[0-9]+$`.
  A `--cases` value with `..`, a newline or a slash cannot be in the allowlist, so `run` returns 3.
  There is no arbitrary `--cases` bypass.
- **No shell interpolation anywhere.**
  `argv` is a list, there is no `shell=True`, no `os.system`, and no `shell` call in the file.
- **No delete surface in the harness.**
  No `rmtree`, no `os.remove`, no `os.unlink`, no `umount`.
  The only signal in the whole file is the one `killpg` above.
- **Per-case directories are immutable per attempt.**
  `prepare_dir` raises `FileExistsError` if the path exists and `RuntimeError` if it is not empty, and it never repairs or reuses.
  Nothing cleans up a borrowed or shared path, because there is no cleanup at all.
- **A missing prerequisite is genuinely fail closed.**
  Control A returned 2 with the correct reason and four blocking entries, and independently rediscovered that `mkfs` and `xfs_io` are absent from a non-root `PATH` on this Mac.
  There is no code path from a missing prerequisite in `STARTUP_GATE` to PASS.
- **Allowlist drift blocks a run.**
  `run` returns 3 on drift, confirmed both by control B's first attempt and by the existing unit test.
- **The declined bypasses were the right call.**
  Planting an executable at `ltp/fsstress` and hand-writing `include/config.h` would both have manufactured a pass, and the document declines both at lines 88-96 with correct reasoning.
  I found no stub helper, no faked `config.h`, and no container install anywhere in the change.
- **`FSTYP` is empty on both arms and `TEST_DEV` is a directory on both.**
  No `mkfs` call is reachable from either arm, and nothing in the change targets a raw device.
  `README.fuse` sanctions that shape for a non-device `TEST_DEV`.
  The native arm in the builder's evidence is a directory on ext4 on `/dev/sda2`, which is context for the backing filesystem, not a device target.

## g5 status

**UNMEASURABLE. Zero measured assertions. Unchanged by this review.**

The builder's refusal is correct and I reproduced its shape.
Every generic case sources `common/rc`, which sources `common/config`, which exits when `$here/ltp/fsstress` or `$here/ltp/fsx` is missing.
Those two are built from `ltp/*.c` through autoconf-generated `include/config.h` and `include/builddefs`, and the tree ships neither.
`autoconf`, `automake`, `libtool` and `m4` are absent from that host, so the tree cannot be built there without installing packages, which is out of scope for the lane.

`mkfs` and `xfs_io` being installed and merely invisible on a non-root `PATH` is not permission and not a capability.
Eight of ten gate entries present still leaves the two helper binaries absent, and `common/config` exits on the first miss.
The gate reaches the same conclusion independently, which is the correct outcome.

The six allowlisted ids are 005, 236, 245, 309, 360, 755, and their allowlist sha reproduces exactly.
Six of the group, and 755 is documented by the builder as unable to fail by its own logic, so at most five of the six could carry a real assertion.
Even with the prerequisite block lifted, this subset cannot carry a gate.

Issue #101 is open, `closedAt` is null, and PR #100 contains no closing keyword.
The timeline shows one cross-reference from #101 into #100 and no close event, so merging #100 would not close the blocker.
That is the correct state and I recommend keeping it that way.

## Provenance of the review

| thing | value |
|---|---|
| reviewed head | `6a8075aedd53a69d6f6735acace5b6817f771290` |
| PR | #100, OPEN, head matches, 3 checks passed 0 failed (`check (ubuntu-latest)`, `check (macos-latest)`, `linux-fuse`) |
| CI | read once at this SHA, no poll, no dispatch, no rerun |
| issue | #101 OPEN, `closedAt` null |
| allowlist ids | 005, 236, 245, 309, 360, 755 |
| allowlist sha | `5eab2e8a473b26613e279266bfb1812426e2a8bfbd256aef2eae9920cb2c6671`, reproduced from the six ids |
| reviewed files | `bench/xfstests_gate.py` 706 lines, `bench/test_xfstests_gate.py` 408 lines, `bench/xfstests-allowlist.txt`, `docs/verification/ready-g5.md` 192 lines |
| reviewer controls | `bench/out/ready-g5-critic/arm_probe.py`, ignored and local |
| unit log | `bench/out/ready-g5-critic/unittest.log`, 73 tests, exit 0 |

CI being green at this SHA does not contradict this BLOCK.
The three checks do not exercise arm placement, transitive `common/*`, source pinning, or the empty-log pass path, which is why all four survived a green run.

## What this review deliberately did not do

- No SSH to moonscape, no interaction with the builder's daemon 899604, store, socket or mount.
  Those were left running and untouched, as instructed.
  The other workers' ssh sessions that were already in flight were not signalled, inspected or disturbed.
- No install, no sudo, no sysctl, no capability change, no device format, no container, no shared-mount change.
- No real xfstests case attempt and no new mount.
  The missing prerequisites are a reported BLOCK, so no full case attempt was warranted.
- No workflow dispatch, no rerun, no runner configuration change.
- No commit, no push, no merge, no lease return.
- No claim about power loss, reclaim, or quiet performance.
  Nothing in this change touches those paths and nothing here measures them.
- No claim that cowfs is or is not xfstests conformant.

## Required before #100 merges

1. Place each arm's per-case directory inside its own `--native-root` or `--cowfs-root`, assert the containment in code, and test that a same-device arm pair is refused.
   This is the blocker.
2. Add a positive assertion check so a case with an empty or assertion-free log is INVALID, never PASS.
3. Give the native-arm-failure case its own tally, and refuse PASS whenever either arm's log carries a skip signature.
4. Pin the tree sha as machine-readable data in the allowlist, compare it, refuse a dirty tree, and record a per-case source hash for every executed case.
5. Hash and pin the `common/*` files an allowlisted case pulls in, or refuse the case.
6. Probe `capabilities_absent` or stop recording it.
7. Make `classify` exit 3 on drift, and fix the two places in the documentation that already claim it does.
8. Make a missing probe case an UNMEASURABLE 2 with evidence written, not a traceback with exit 1.
9. Give `report` a nonzero exit on FAIL or INVALID.
10. Pin the expected case count and tree sha that `--require-full` compares against, and include suffixed `tests/generic` entries in the denominator.
11. Remove the unused `os` import so `ruff --select F,E9` is clean.
12. Get the lead's explicit decision on the `killpg`, since it contradicts a categorical standing rule even though its blast radius is correct.