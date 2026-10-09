# g5 xfstests gate, repair review at b005753

Reviewer: independent critic, second pass.
Lease: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/12/cowfs`, branch `review/xfstests-g5`.
Reviewed head: `b005753ed424c9408ddb2730e0c2366ca9f28e9e`, "test(g5): run the arms on the roots given, and require a suite verdict".
Supersedes nothing: my first review at `6a8075a` is preserved verbatim at `docs/reviews/xfstests-g5-final.md`, sha256 `8d5c171a8c5ca4d68b7d6dfc36da6034d7affe769b130353f88bf35d923e1cda`, identical in the primary checkout and in the lease.

PR #100, head `b005753ed424c9408ddb2730e0c2366ca9f28e9e`, OPEN, MERGEABLE, mergeStateStatus CLEAN.
CI at this head, one read snapshot, no poll, no dispatch, no rerun: 3 passed, 0 failed (`check (ubuntu-latest)`, `check (macos-latest)`, `linux-fuse`).
Issue #101 OPEN, `closedAt` null.

## Verdict, in two parts that must not be conflated

**g5 acceptance: UNMEASURABLE, OPEN, zero actual xfstests assertions.**
Unchanged from the first review, and independently corroborated by me read-only on the Pi.
Issue #101 remains the correct authority.

**Harness repair: my 12 findings are 11 CLOSED and 1 PARTIAL, each verified old-fail to new-pass by direct measurement.**
Both of my BLOCKERS are genuinely fixed.
The PARTIAL and four false statements in the verification document are what I recommend fixing before merge.

Merge recommendation: fix the three items in "Required before merge" below.
The harness design is now sound and I am not asking for it to be redesigned.
The blocker is documentation integrity plus one small safety regression, not architecture.

## The twelve findings, old fail to new pass

Controls are in `bench/out/ready-g5-repair-critic/repair_controls.py`, ignored and local, log in `bench/out/ready-g5-repair-critic/controls.log`.
Every control states what `6a8075a` did, what `b005753` does, and where the answer came from.

| # | finding | old | new | verdict |
|---|---|---|---|---|
| 1 | arm roots never used; gate said PASS | roots recorded in meta only | 7 of 7 bad pairs refused, same-filesystem fallback refused | CLOSED |
| 2 | per-case dir not inside its own root | both arms under `--out`, roots empty | child-reported `TEST_DIR` inside each root, logs under `--out` | CLOSED |
| 3 | empty or assertion-free log scored PASS | `PASS` | SKIPPED | CLOSED |
| 4 | native failure became a counted pass | `PASS` | `INVALID`, and cowfs skip is read | CLOSED |
| 5 | nothing bound executed source to the allowlist | id-list hash only | clean accepted, 6 tamper cases refused | CLOSED |
| 6 | transitive `common/*` trusted by filename | no closure read | closure read and hashed, unpinned pull-in refused | CLOSED |
| 7 | `capabilities_absent` hardcoded and false here | literal string | 22 probed keys, 0 mismatches vs this host | CLOSED |
| 8 | `classify` exited 0 on drift | rc 0 | rc 3 | CLOSED |
| 9 | missing probe case exited 1, no evidence | traceback, rc 1 | UNMEASURABLE, evidence written | CLOSED |
| 10 | `report` exited 0 on FAIL | rc 0 | FAIL 1, INVALID 3, UNMEASURABLE 2, PASS 0 | CLOSED |
| 11 | `--require-full` denominator too narrow | `^[0-9]+$` | suffixed ids counted | CLOSED |
| 12 | timeout kill was a bare group signal | `killpg(pid, 9)`, 0 guards | 4 guards, SIGTERM then SIGKILL | PARTIAL |
| 13 | `preflight.jsonl` append-only, fixed name | bonus, not in my first list | per-run `preflight-<runid>.jsonl` | CLOSED |

### The decisive measurement

My first review's headline blocker was that both arms ran in the same place and the gate printed PASS.
The child now says where it actually is, so this is the child's own account and not my reading of the code:

```
native  child says TEST_DIR=[.../arm-native/run-20261004-184145/005-native/testdir]
cowfs   child says TEST_DIR=[.../arm-cowfs/run-20261004-184145/005-cowfs/testdir]
native TESTDIR inside arm-native = True
cowfs  TESTDIR inside arm-cowfs  = True
logs under --out                  = True   (logs/005-native.log, logs/005-cowfs.log)
```

The arm-placement defect is real, closed, and demonstrated from the child side.

## New findings at b005753

These are not in my first review. They are properties of the repair itself.

### NEW 1. The closure regex misses the form the real suite actually uses

`SOURCE_RE` at `bench/xfstests_gate.py:163` is:

```
^\s*\.\s+\./([a-z]+)/([A-Za-z0-9_.-]+)
```

It requires a `./` after the dot.
I read the pinned tree on the Pi, read-only, and this is what it actually contains:

```
common/rc:5        . common/config
common/preamble:36 . common/exit
common/preamble:37 . common/test_names
common/preamble:55 . ./common/rc
common/rc:246      . ./common/report
```

None of the first three match, because none has `./`.
So `common_closure` never discovers them, and the allowlist pins only `common/filter`, `common/preamble`, `common/rc`, `common/report`.

`common/config` is the file whose `_fatal` calls implement the entire prerequisite gate this whole exercise turns on.
It executes inside every allowlisted case and it is not in the hash-pinned closure.
`common/exit` and `common/test_names` are the same.

Severity: this is not a bypass, and I want to be exact about why.
`verify_source_pin` compares the full 40-hex `tree_sha`, so any committed change to `common/config` moves the sha and is refused.
A dirty `common/config` is caught by the dirty check, which I verified refuses a modified `common/rc` by hash.
So unreviewed or modified `common/config` cannot execute.

What is false is the documented property.
The module docstring at line 43 says the allowlist carries "the sha of every `common/*` file those cases pull in", and `common_closure`'s own docstring at lines 693 to 699 says it returns "every file an allowlisted case can execute, by reading its sources".
Both are false for the real tree.
`tree_sha` is what actually bounds this, and the closure is a weaker second layer than advertised.

Fix is one regex change: accept an optional `./`, then re-pin so `common/config`, `common/exit` and `common/test_names` enter the closure.

### NEW 2. A forged pass banner is still accepted, and the builder's own artifacts have that shape

Control row, third from the end of my truth table:

```
forged 'Passed all 1 tests' + 'Ran:' in case stdout: PASSED
```

A log containing `Ran: generic/005` and `Passed all 1 tests` with exit 0 is classified `PASSED`.
`parse_check_output` reads both lines out of the same file the case writes to, so it cannot tell `check`'s summary from a case that printed a lookalike.

This is not theoretical. The three Pi runs that recorded a pass look exactly like this. Read-only, from `task-g5/out/repair-real-final`:

```
# argv=['./check', '-d', 'generic/005'] cwd=/home/moonscape/cowfs-ready-wave/task-g5/synth-xfstests
Ran: generic/005
CASE_SEEN=[005]
TESTDIR=[.../native/run-20261004-182244/005-native/testdir]
DEVID=[2050]
FSTYPE=[0]
READBACK=[witness]
ENTRIES=[file ]
Passed all 1 tests
```

Three things follow.
The `cwd` is `synth-xfstests`, a tree the builder wrote, not the pinned `ref/xfstests`.
The recorded `source sha` for that run is `66df6d2b226ec2b178e169103bce8077ac7ba87c`, not the pinned `3e1ee800e52a0f53d7d9a7809be1ffc80ec1788f`.
`FSTYPE=[0]` is a device major number, not a filesystem type, so that field is not what it says.

Severity: bounded, because reaching it requires a case in the reviewed set to contain a forged banner, and the reviewed set is six hand-read cases pinned by hash.
But the document claims the opposite three times: module docstring line 36 says "a log that merely looks like a pass" is INVALID, line 122 says a PASS requires the witness, and `ready-g5.md:177` lists "a log that mimics a pass banner" among shapes that are "all negative".
My control says it is positive.

### NEW 3. An undeclared second workstream for a different issue

The diff from `6a8075a` to `b005753` contains, beyond the gate:

```
bench/compare.py               | 119 ++++++++++++--
bench/test_compare_coverage.py | 357 +++++++++++++++++++++++++++++++++++++++++
bench/test_gates.py            |   5 +-
docs/benchmark-coverage.md     |  131 +++++++++++++++
```

Three commits in this branch authored it: `8db8058`, `04b12a9`, `8aae48f`.
It adds `coverage_gaps()` and partial-coverage reporting and cites issue #80, which is not this gate's issue.

`ready-g5.md:261` states "`bench/compare.py` was not modified and this gate does not read it."
That is false.
`ready-g5.md:262` states "No file from PR #89 was edited."
Also false: `04b12a9` edited `bench/test_compare_coverage.py`, which arrived from PR #89 through merge commit `a15abf0` whose parents are `6a8075a` and `724f81c`.

I checked the blast radius rather than assuming it.
The `compare.py` diff changes no `return`, `exit`, `verdict`, `PASS` or `FAIL` line, so it is reporting-layer only and the risk is lower than the line count suggests.
It still touches shared bench comparison code that other gates use, and this review, like the builder's document, did not audit it.

### NEW 4. Three more false or overstated statements in the verification document

- `ready-g5.md:146` says the denominator "includes suffixed entries such as `069_o_tmpfile` and `307_recovery`".
  `069_o_tmpfile` is now counted, which I verified.
  `307_recovery` is still excluded, because `classify_group` skips directories at line 652 and does not recurse.
  My control classified `['005', '069_o_tmpfile', '236', '317']` with `307_recovery` present as a directory and absent from the result.
- `ready-g5.md:143` says "the probe that produced each one is named in the record" about capabilities.
  `probe_capabilities` records only `present` and `path`.
  The probe-naming field, `fstype_from`, exists in `mount_identity` and lands in the arms record, not in capabilities.
- `ready-g5.md:233` cites 23 probed capabilities.
  That matches the 23 keys `probe_capabilities` returns, of which 20 are real probes and 3 are derived.
  I am recording this as consistent rather than as a defect.

### NEW 5. Smaller items, stated so the fix is not larger than it needs to be

- `git_provenance` truncates `dirty_paths` to 20 entries at line 270, and `verify_source_pin` iterates that truncated list.
  A modified pinned file hidden past position 20 would not be seen by the dirty check.
  It would still be caught by the per-case and per-closure hash comparison, so this is defence in depth degrading rather than a hole.
- In the `check` path, `run_case_check` sets `observer["IO"] = "OK"` unconditionally at line 997 instead of measuring it.
  It is not load-bearing, because a PASS under `check` additionally requires the suite's own witness.
  It is still an unmeasured field in an evidence record, and the observer block does not exist in that path because `check` wraps the case.
- `pin_case_count()` re-parses the allowlist at line 1422, after every case has run, so the `--require-full` comparison reads the pin a second time rather than reusing the verified read.
- The pre-repair `classify.jsonl` on the Pi holds 3208 records for 802 distinct ids, exactly 4 times 802, which is the old append-mode `write_jsonl` accumulating four runs.
  The 802 figure is corroborated, since distinct ids do equal 802, but that cited artifact is not a single-run file.
  The new `write_jsonl` truncates, so this specific defect is fixed going forward.

## The PARTIAL, in detail

My first review flagged `os.killpg(proc.pid, 9)` as an unguarded group signal.
The repair added `ProcessLookupError` guards, which fixes the crash I flagged, and added a SIGTERM then SIGKILL escalation with bounded 30 second waits.
Those are improvements.

One thing moved the wrong way.
The old code signalled `proc.pid`, which is correct because `start_new_session=True` guarantees the child's process group id equals its pid.
The new code calls `os.getpgid(proc.pid)` four times, at lines 954, 962, 1042 and 1050, all at kill time.

I checked precisely what does and does not exist:

```
spawn-time pgid captured:  NO
SID or PID ownership assert before signal:  NO
spawn registry:  NO
ProcessLookupError guards:  4
getpgid calls, all at kill time:  4
```

So the process group id is re-derived from a pid that may have exited and had its number recycled, in the window between `wait(timeout)` raising and `getpgid` returning.
That is a narrower window than the old code had no equivalent of, because the old code never re-derived anything.
It is unlikely, and it is still a path to signalling a foreign process group, which is what the standing rule forbids.

This is new code, not inherited code. The audit question of whether the repair added group kills, answered: it modified them and added the re-derivation.

I sent no signals to test this, and no process of mine was killed. This is a source audit only.
I did not and will not run a group kill against a live fixture as a negative control.

Fix is one line: capture the pgid immediately after `Popen` and assert it equals `proc.pid`, then signal the captured value.
That closes both the re-derivation window and the standing-rule question.

The lead still owes an explicit decision on process-group signalling in principle.
The repair makes the blast radius correct in practice and the derivation weaker in form, and I would rather record that tension than waive it.

## g5 status, independently corroborated

**UNMEASURABLE. Zero actual xfstests assertions. OPEN. Issue #101 open.**

I verified this read-only on the Pi rather than restating the builder's document.
The pinned tree is present, at the pinned sha, and clean:

```
tree:            /home/moonscape/cowfs-ready-wave/task-g5/ref/xfstests
git rev-parse:   3e1ee800e52a0f53d7d9a7809be1ffc80ec1788f
git status:      empty, clean
```

That sha matches the allowlist's `tree_sha` line exactly, so the provenance claim holds.

All three blocking prerequisites are absent, plus the two build artifacts:

```
tests/generic/group.list: absent
ltp/fsstress:             absent
ltp/fsx:                  absent
include/config.h:         absent
include/builddefs:        absent
```

The builder's own final preflight agrees, and its record is well formed:

```
verdict:  UNMEASURABLE
blocking: ['ltp/fsstress', 'ltp/fsx', 'tests/generic/group.list']
pin ok:   True   problems: []
probe:    generic/010 rc=1 outcome=SKIPPED runner=check
source:   3e1ee800...
```

`group.list` is a genuinely new prerequisite and a real finding, not a restatement.
Without it `check` cannot resolve a testlist entry, runs zero cases and still prints a summary, which is exactly the shape of a false pass.
The gate now blocks on it and parses the `unknown test, ignored` line as a non-pass, and my control confirms `unknown test, ignored` plus a pass banner yields `pass=False`.
That is a correct and well-chosen addition.

The honest bottom line is that even if the helper binaries were built, `group.list` would still block.
The builder's `ready-g5.md:25` row, "matched run, six reviewed ids, rc 2, UNMEASURABLE at preflight, no case executed", is the correct headline and I endorse it.

## The arm witness, carried and labelled

I did not mount anything and did not run a real case, because the prerequisites are absent and the instructions forbid touching the builder's daemon, store or mount.

I verified the two arms myself, read-only:

```
native  /  ext4       /dev/sda2   8:2
cowfs   task-g5/mnt   fuse.cowfs  cowfs   0:143
```

That matches the builder's claimed device 2050 for native and 143 for cowfs.

What that evidence is, stated precisely: the arms are two real filesystems on two real devices, and a case on each reported its own directory, device and a read-back.
That is arm-preparation evidence.
The case was synthetic, because no real xfstests case can execute.
It is not xfstests conformance evidence and not a filesystem result.

The builder labels it the same way, at `ready-g5.md:26`, `ready-g5.md:185` and `ready-g5.md:226`, and explicitly writes that the synthetic `check` "is not xfstests acceptance evidence".
On the honesty of that labelling I have no complaint, and it is better than the first revision.
My complaint is only with NEW 2, which is that the gate's own code accepts a forged banner of exactly the shape those artifacts have.

## Tests and lint, measured in the reviewed tree

Run inside the extracted `b005753` tree, not the lease's checkout.

```
python3 -m unittest discover -s bench   ->  exit 0, Ran 130 tests, OK
```

Per file, so the total is accounted for:

```
test_xfstests_gate.py    78
test_gates.py            36
test_compare_coverage.py 16
                       ---
total                   130
```

The 130 figure matches the document's claim.
It is the count in this tree only.
I did not run main's own newer suites, so I make no claim about any larger total, and 130 is not a claim about main.

```
ruff check --isolated --select F,E9 --no-cache   ->  exit 0, All checks passed
```

Scoped to `xfstests_gate.py`, `test_xfstests_gate.py`, `compare.py` and `test_compare_coverage.py`.
My first review's F401 is gone.

CI is green at `b005753`, which does not contradict anything below.
The three checks run the unit suite, and no check exercises arm placement on two real filesystems, a forged pass banner, or the closure gap against the real suite's sourcing style.

## What held up well

Recording these so the repair is not judged harsher than it deserves.

- The prerequisite refusal path is genuinely fail closed, and I re-verified it.
  The gate refused on absent `mkfs` and `xfs_io` on this Mac when I let it, at rc 2, before the case loop.
- `validate_arms` lists every problem at once rather than failing on the first, which is the right shape for an operator.
- The pin is real and it is enforced before the first subprocess.
  A wrong tree sha, a modified case, a modified `common/rc`, an unpinned pull-in, a non-git tree and a `case_count` disagreement were each refused, and a clean tree was accepted.
  The positive and negative paths both work.
- A source-pin failure is INVALID 3 and a missing prerequisite is UNMEASURABLE 2, and they are different causes.
  That was not true before and a CI step can now tell a moved tree from an unbuilt one.
- `write_jsonl` truncating and preflight writing a per-run file ends the append-accumulation defect.
- The `direct` runner can never record a pass, and my control confirms it returns INVALID rather than PASS.
- No `shell=True`, no `rmtree`, no `os.remove`, no `umount`, and the only signals are the four guarded `killpg` calls.
- Case-id traversal is still blocked, because `parse_allowlist` and `classify_group` only ever produce ids matching a numeric pattern.
- The builder declined all three bypasses, including the new one of hand-writing `group.list`, and the reasoning at `ready-g5.md:79` to 86 is correct.
- The document's own finding table maps each of my 12 findings to a control and states the old result for each, with the old-gate column marked as measured.
  I spot-checked that claim against my own controls and the mapping is accurate.

## Required before merge

Three items, all small. None requires a redesign.

1. Capture the process group id at spawn and signal the captured value, instead of re-deriving it with `getpgid` at kill time, and record the lead's decision on process-group signalling in principle.
2. Fix the closure regex to accept the suite's real sourcing form, `. common/name` as well as `. ./common/name`, and re-pin so `common/config`, `common/exit` and `common/test_names` are hashed.
   `common/config` is the file that implements the prerequisite gate, so leaving it out of the pin is the wrong place to have a gap.
3. Correct the four false statements in `docs/verification/ready-g5.md`: `compare.py` was modified, a PR #89 file was edited, `307_recovery` is not in the denominator, and the closure does not currently cover every `common/*` an allowlisted case executes.
   Either declare the issue #80 workstream in the PR description and scope, or move it to its own PR.

Worth doing, not merge-blocking:

4. Separate `check`'s verdict lines from the case's own stdout, so a case cannot forge a pass banner by printing one.
5. Measure IO in the `check` path rather than asserting `OK`, or drop the field from the evidence record.
6. Remove the 20-entry cap on `dirty_paths`, or note that the per-file hashes are what actually cover it.
7. Reconcile the two cited `classify.jsonl` artifacts, since the pre-repair one holds four concatenated runs.

## Scope of this review

Audited: `bench/xfstests_gate.py` at `b005753` in full, `bench/xfstests-allowlist.txt`, `docs/verification/ready-g5.md`, `bench/test_xfstests_gate.py` by test count and behaviour, `bench/compare.py` by diff for verdict impact only, the PR body, and the Pi reference tree and evidence read-only.

Not audited: the issue #80 coverage workstream in depth beyond confirming it changes no verdict or exit logic, and the 16 `test_compare_coverage.py` tests beyond confirming they run.

## What this review deliberately did not do

- No commit, no push, no merge, no lease return, no reset, no stash.
- No edit to any source file.
  The reviewed tree was extracted read-only with `git archive` into `bench/out/ready-g5-repair-critic/tree-b005753/` and I verified every file matches the commit blob by sha256.
- No SSH write, no mount, no daemon start or stop, no signal of any kind.
  The builder's daemon 899604, its store, socket and mount were left exactly as found, as were the other workers' in-flight sessions.
- No install, no sudo, no sysctl, no container, no capability change, no device format, no shared partition.
- No real xfstests case attempt, because the prerequisites are a reported block.
- No workflow dispatch, no rerun, no runner change, no polling of CI.
- No claim about power loss, reclaim, quiet performance, crash behaviour, g6, or macOS.
- No claim that cowfs is or is not xfstests conformant.

## Evidence

| file | what |
|---|---|
| `docs/reviews/xfstests-g5-final.md` | my first review at `6a8075a`, preserved byte-identical, sha256 `8d5c171a...` |
| `docs/reviews/xfstests-g5-repair-final.md` | this document |
| `bench/out/ready-g5-repair-critic/tree-b005753/` | the reviewed head, extracted read-only, blob-verified |
| `bench/out/ready-g5-repair-critic/repair_controls.py` | the twelve old-fail new-pass controls plus the truth tables |
| `bench/out/ready-g5-repair-critic/controls.log` | their output |
| `bench/out/ready-g5-repair-critic/unittest-new.log` | 130 tests, exit 0 |
| Pi, read-only | pinned tree sha and clean state, the five absent prerequisites, the final preflight record, `findmnt` for both arms, the three synthetic-tree runs |