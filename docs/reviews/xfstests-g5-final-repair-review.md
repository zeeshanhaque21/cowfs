# g5 xfstests gate, final repair review at 445286b

Reviewer: independent critic, third pass, resuming the same held reviewer lease.
Lease: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/12/cowfs`, branch `review/xfstests-g5`, still at `6a8075a`, untouched.
Reviewed head: `445286b8e1c861e59af3d25d7a0b0b2a2998b46e`, "test(g5): read the closure the suite writes, and bind a pass to the suite's own runner".
Previous review head: `b005753ed424c9408ddb2730e0c2366ca9f28e9e`, preserved at `docs/reviews/xfstests-g5-repair-final.md`.
First review head: `6a8075a`, preserved at `docs/reviews/xfstests-g5-final.md`.

PR #100, head `445286b8e1c861e59af3d25d7a0b0b2a2998b46e`, OPEN, MERGEABLE, mergeStateStatus CLEAN.
CI at this head, one read snapshot, no polling, no rerun, no dispatch: 3 passed, 0 failed (`check (ubuntu-latest)`, `check (macos-latest)`, `linux-fuse`).
Issue #101 OPEN, `closedAt` null.
No closing phrase in the PR body or in any added commit message, including negated forms.

## Verdict

**The four findings I raised at `b005753` are CLOSED. Three of four verified by direct measurement on this host; the fourth needed the Pi and I derived it read-only from the real tree.**

**g5 acceptance: UNMEASURABLE, OPEN, zero actual xfstests assertions. Unchanged.**

One new residual, disclosed rather than papered over: the closure regex still misses four source forms, and a chain deeper than `CLOSURE_DEPTH` is truncated without an unresolved marker.
Neither is a bypass, and I explain why below.

Merge recommendation: this is mergeable on the g5 lane's own terms.
The four findings are genuinely closed and the two substantive false-pass paths from my second review are shut.
I would not block on the residuals, and I would not describe the closure as complete.

## The four findings

Controls in `bench/out/xfstests-g5-final-critic/`, ignored and local.

| # | finding I raised at b005753 | verdict | how it was verified |
|---|---|---|---|
| F1 | kill-time `getpgid` re-derivation, no spawn registry, no ownership proof | CLOSED | 11 controls, source audit plus live owned-child signal |
| F2 | closure regex missed `. common/config`; depth 4 | CLOSED | derived against the real tree on the Pi, read-only |
| F3 | a forged `Ran:` plus `Passed all N tests` was accepted | CLOSED | 21 full-CLI runs across 10 forge shapes |
| F4 | the pin was caller-selectable, so a stand-in could pass as acceptance | CLOSED | own-pin and shipped-pin runs of the same stand-in tree |

### F1. Spawn registry and kill-time recheck: CLOSED

The old code held no registry, re-derived the process group at kill time, and never read the kernel's record.
The new code registers the child at spawn, before `wait`, and refuses to signal unless four things line up at the moment of the signal.
I verified each refusal:

```
no spawn registry entry for this pid
the child handle was already reaped
no child handle is held for this pid
start time differs: recorded N, now M, so this pid was reused
process group or session changed
the child is in the harness's own process group or session
no spawn-time identity was recorded, so identity cannot be proven
```

`proc_identity` reads `/proc/<pid>/stat` for `pgrp`, `sid` and `starttime`, plus `/proc/<pid>/cmdline` for `argv`.
The `argv`, `cwd`, pid and spawn identity are all recorded in `register_child` before `proc.wait` is called, which is the ordering the finding asked for.

The signal surface is now single-pid.
`os.killpg(` appears nowhere in the file; `os.kill(pid, sig)` is the only send.
Escalation is SIGTERM then SIGKILL, with ownership re-proved before each, and `ProcessLookupError` and `PermissionError` both recorded as refusals rather than raised.

Two properties I want to state precisely because they are where a quiet overclaim would live.

**A descendant is not contained, and the code says so.**
The comment block states it directly: "The signal is sent to the single pid the harness holds a handle for, not to a process group. That is the standing instruction, and it also means a descendant is not contained by the signal; that is recorded in the case record as `descendants_contained: false` rather than papered over."
`descendants_contained` appears 3 times in the file.
The old code's group kill implicitly covered grandchildren; this code does not, and it declines to pretend otherwise.

**The log is flushed and fsynced before any signal.**
In `run_case_check` the timeout branch calls `fh.flush()` then `os.fsync(fh.fileno())` before `stop_child(entry)`.
`append_jsonl` also flushes and fsyncs per record.

One honest limit of my own evidence: **this host is darwin and has no `/proc`, so `proc_identity` returns `None` and every ownership check refuses here.**
That is the fail-closed direction, and it is why the repository's own spawn test skips on this host, which I confirmed rather than assumed: the suite ran 162 tests with `skipped=1`.
The positive case, where a genuinely owned child is admitted and signalled exactly once, is therefore verified on the Pi and not here.
I sent no signal to any borrowed process.
The only children signalled in my controls were ones my own script spawned, and I cleaned them up by single verified pid.

### F2. The closure now reads what the suite writes: CLOSED

This is the finding that needed the real tree, so I read it read-only on the Pi and reimplemented the walk there rather than assuming.

The real sourcing lines, at `ref/xfstests` pinned at `3e1ee800`:

```
common/rc:5        . common/config
common/preamble:36 . common/exit
common/preamble:37 . common/test_names
common/preamble:55 . ./common/rc
common/rc:246      . ./common/report
```

Three of those five have no `./`, which is exactly what the old regex required.
Measured against the real tree, with the new regex and `CLOSURE_DEPTH = 8`:

```
005: NEW=16 OLD=4
236: NEW=15 OLD=3
245: NEW=16 OLD=4
309: NEW=16 OLD=4
360: NEW=16 OLD=4
755: NEW=15 OLD=3
unresolved: none
```

The union is 16 files, against an old pin that named 4.
That reproduces the document's claim independently: old pin 4 files, six cases reaching a union of 16.

The three newly reached files are the important ones: `common/config`, `common/exit`, `common/test_names`.
`common/config` is the file whose `_fatal` calls implement the entire prerequisite gate, and it executes inside every reviewed case.
The new allowlist pins 94 `common` lines covering 16 distinct paths, and those 16 are exactly the real closure, so the shipped pin is a superset of what the walk reaches and not merely a hand-written list.

The old-shape pin against the new verifier produces the refusal the document describes.
Reconstructing the six cases against a 4-entry pin gives 18 `pulls in unpinned` problems naming the three missed files, and the document reports 72 problems for the same matrix on the Pi, which counts all refusal classes rather than only this one.
The direction and the named files agree; the totals are different measurements and I am not treating them as the same number.

`CLOSURE_DEPTH = 8` is a module constant and `common_closure(depth=None)` defaults to it, so pin generation and pin verification walk the same graph.
That is what stops a pin claiming coverage the verifier does not check.

An unresolvable source line is recorded as `__unresolved__` and refuses, rather than reading as a clean result.
I confirmed the mechanism exists in `verify_source_pin` at the `rel == "__unresolved__"` branch.

### F3. Forged summaries cannot become a pass: CLOSED

My second review demonstrated that a case printing `Ran: generic/005` and `Passed all 1 tests` with rc 0 was classified `PASSED`.
That path is shut, and I tested it the hard way: through the full `gate.run` CLI, not the parser.

The structural change is that the witness is read from the runner's own file descriptors, never from the case's log.
`run_case_check` opens `check-streams/<case>.out` and `.err`, points `check`'s stdout and stderr at them, and builds `witness_text` from those two descriptors only.

That separation alone is not sufficient, and I checked why rather than assuming.
The real `check` invokes the case as `bash -c "exec ./$seq"` with no redirect, so the case inherits the runner's stdout and its output really does land in the witness stream.
So the positional rules are what make a forged line inert:

- exactly one `Ran:` line; a second one means the stream is not the runner's alone, and the run is refused
- the summary read is the last one in the stream, never the first
- the count in the summary must equal the number of ids the runner named
- the ids named must be exactly the ids requested: none missing, none extra, none duplicated

Results across 10 forge shapes, each run twice, once under a matching caller-supplied pin and once under the shipped pin:

| forge shape | own pin | shipped pin |
|---|---|---|
| forged summary, correct ids | INVALID | refused at preflight |
| forged summary after a real `Ran:` | PASS | refused at preflight |
| runner prints real line, case prints a second `Ran:` | INVALID | refused at preflight |
| plausible matching id and count, case forges banner | PASS | refused at preflight |
| case forges both `Ran:` and summary inside a live runner | INVALID | refused at preflight |
| case forges summary, then a failure summary | PASS | refused at preflight |
| runner names 2 ids, summary says 1 | INVALID | refused at preflight |
| no `group.list`, runner claims a pass | refused | refused |
| pass summary but nonzero exit | refused | refused |
| runner silent, case prints a full-looking pass | INVALID | refused |

Three of those reach PASS under the caller's own pin, and that needs stating accurately rather than as a bypass.
In each of the three the runner itself genuinely printed `Passed all 1 tests` and genuinely exited 0, so the gate's verdict agrees with the runner.
The forgery was redundant, not load-bearing.

The bypass attempts are the ones that matter, and I built a second harness for them using the real routing.
A stand-in runner that reproduces `check:839` and `check:848` exactly, running the case with its output in the runner's own stream:

```
case FAILS (status=1), forges "Passed all 1 tests"                    -> no PASS
case FAILS silently, forges pass banner only                          -> no PASS
case FAILS, forges pass, spawns a background printer                  -> no PASS
case forges both Ran lines                                            -> INVALID
clean case, no forge                                                   -> PASS   (correct)
clean case that genuinely fails                                       -> no PASS (correct)
```

Zero bypasses.
The forged banner cannot become the last summary line, because the runner's own summary is written after the case returns, and the runner's exit code must agree.

`direct` still cannot reach PASS: it returned INVALID with a clean rc 0, as designed.

### F4. The pin is shipped, not caller-selected: CLOSED

`is_synthetic_tree` reads `reviewed_pin()`, which parses `REVIEWED_PIN_FILE`, a fixed path beside the gate.
There is no `--allowlist` CLI flag; the only mentions of that string are in comments explaining why it is not consulted.
The decision therefore cannot be moved, renamed or re-pinned by a caller.

Verified with the shipped pin in place, against a stand-in tree at a different sha:

```
shipped pin tree_sha:      3e1ee800e52a0f53d7d9a7809be1ffc80ec1788f
synthetic tree sha:        55935cf079bfe5e5d611e7ad5c1dd2d0c1e05e6a
is_synthetic_tree(tree):   True
```

Same stand-in tree, two pins:

```
own matching pin  -> acceptance=True   (harness proof; the tree IS what that pin names)
shipped pin       -> acceptance=False, synthetic=True, refused at preflight
```

The receipt also binds the executor's bytes, not just the tree.
The allowlist carries `runner check 53dd21653ec6eabd15b3d7e73b821b9fcc98202d3c791a65768329897bd69e18`, and `check_sha_is_pinned` compares against that.
When I replaced `check` after generating the pin, the run was refused.
It also binds `group.list`: the receipt reads the suite's own selection file and refuses when it is missing or does not select the requested case.

So an approved synthetic runner does not become genuine suite acceptance.
Under the shipped pin it cannot, and under a caller's own pin it is labelled harness proof by construction.

## Residual findings, disclosed not waived

### R1. The closure regex still misses four source forms

```
'. ./common/x && true'        NO MATCH
'. ${VAR}/common/x'           NO MATCH
'. ./common/rc # comment'     NO MATCH
'. ./common/x; do_thing'      NO MATCH
```

The new pattern anchors with `\s*$`, which is what makes the bare `. common/config` form work and what excludes anything trailing on the line.
The cost is that a source line with a trailing command, a variable path, or a trailing comment is not followed.

Severity: low, and not a bypass.
The real reviewed cases produce no unresolved marker, so on this tree the closure is complete by the gate's own measure.
A shell construct the regex cannot follow would be a file the gate has not read, which is exactly the gap `tree_sha` backstops: any committed change to such a file moves the tree sha and is refused.
The honest statement is that the closure is a lower bound on what executes, derived from a lexical scan of source lines, not a complete-closure proof.

I checked whether the document overclaims this.
It does not: it says "A source line this scan cannot resolve to a file is recorded as unresolved and refuses, rather than being read as a clean result", and it lists the regex change without asserting the scan is exhaustive.

### R2. A chain deeper than CLOSURE_DEPTH is truncated silently

```
chain depth  7: files read=  7  unresolved=NONE
chain depth  8: files read=  7  unresolved=NONE
chain depth  9: files read=  7  unresolved=NONE
chain depth 12: files read=  7  unresolved=NONE
```

`common_closure` stops after `depth` rounds.
The unread tail does not produce an `__unresolved__` entry, because those files exist and resolve fine; they are simply never visited.
So the refusal I verified in F2 covers an unresolvable name, not an unvisited one.

Severity: low on this tree, and I checked what the real tree does rather than guessing.
The deepest reviewed chain is 7 levels, so `CLOSURE_DEPTH = 8` leaves a margin and no reviewed case is truncated.
The document states the same rationale: "the reviewed cases settle at seven levels, and eight leaves the deepest one a margin without pretending to follow an unbounded chain".

Worth a sentence in the document that the bound is a chosen depth rather than a proof of completeness, and worth a marker in the record when the walk stops at the limit with a non-empty frontier.

## Comparator ancestry, preserved

The instruction not to recommend reverting inherited work is correct and I checked it rather than repeating my second review's framing.

`724f81c` (PR #89) is an ancestor of the reviewed head, and both comparator files are blob-identical to main:

```
bench/compare.py               head=42675c46...  main=42675c46...  IDENTICAL
bench/test_compare_coverage.py head=2c0fe255...  main=2c0fe255...  IDENTICAL
```

My second review flagged a `compare.py` change and an undeclared workstream, on the pre-merge base `6a8075a`.
At this head those changes are inherited main, not this lane's, and the document says so at line 333: "The diff against `6a8075a` shows that work because `6a8075a` is the pre-merge base, and this branch merged main at `a15abf0`, whose parents are `6a8075a` and `724f81c`."
Merge commit `a15abf0`, parents `6a8075a` and `724f81c`, which I verified.

No recommendation to revert anything.

The branch also pins the revision it was tested against, not current main.
Main is at `951045f`, this branch at `445286b`, and the branch's own evidence names its head rather than main.

## Documentation claims I re-derived

The denominator section is new and it is the part most likely to be wrong, so I measured it.

```
total entries in tests/generic:            1610
files only:                               1610
directories among them:                      0
bare-numeric case files:                   802
group.list:                               absent
```

The document's table says 1609 entries starting with a digit, 807 build outputs, 802 case files, 0 directories, 0 suffixed ids.
My count of total entries is 1610 against the document's 1609.
The difference is one entry not starting with a digit, which the document's own wording accounts for by describing its 1609 as "entries whose name starts with a digit" rather than all entries.
So the arithmetic is consistent and the denominator of 802 is confirmed.

Two of my second review's false statements are corrected here, and correctly:

- `307_recovery` is now described as a directory that is skipped and is not in this tree, with the earlier claim called wrong twice over.
  I confirmed `307_recovery` is absent from this tree and that `classify_group` skips directories.
- the capability count is broken down as 21 measured, 1 derived, 1 stated, each with a `how` field.
  21 measured is 15 PATH lookups plus 5 tree-file `stat` checks plus 1 `os.getuid()`; I verified that decomposition against `probe_capabilities`.

The `1609 / 807 / 802` figures and the 23-capability breakdown are consistent with what I measured.
I did not independently re-derive the 807 build-output breakdown line by line, and I am not restating it as measured.

## Tests and lint, measured in the reviewed tree

```
python3 -m unittest discover -s bench   ->  exit 0, Ran 162 tests, OK (skipped=1)
```

Per file, collected not added:

```
bench/test_xfstests_gate.py    110   (of which 1 skips on a host with no /proc)
bench/test_gates.py             36
bench/test_compare_coverage.py  16
                              ---
                               162
```

The 1 skip is the ownership test, which skips rather than claiming a pass where `/proc` is absent.
I confirmed that skip is real and not a masked failure, which is the same reason my own F1 positive case moved to the Pi.

The document's counts are 110 gate, 162 discovery, 86 evidence tests across 20 findings, 11 behaviour, and 1 skip.
My measurement confirms 110, 162 and 1.
I did not run `final-repair/evidence_controls.py` or `behaviour_controls.py`, so the 86, 20 and 11 figures are the document's and I am not claiming them as mine.
They are per-file collections that must not be summed with the discovery total, and the document keeps them separate.

```
ruff check --isolated --select F,E9 --no-cache   ->  exit 0, All checks passed
```

Scoped to the four bench files, `F,E9` only.
I make no blanket lint claim beyond those two rule sets.

CI is green at `445286b`, which does not contradict the residuals.
The checks run the unit suite, and no check exercises a source line the regex cannot follow, a chain deeper than eight, or a real `/proc` ownership admission.

## What held up

- `tree_sha` plus the dirty check remain the real backstop for the closure gap, and I re-verified both refuse a tampered tree.
- `is_synthetic_tree` reads the shipped pin, so the synthetic-versus-acceptance decision cannot be moved by a caller.
- The receipt binds executor bytes, `group.list` selection, the case id from the runner's own `Ran:` line, and exact id set membership.
- The witness is read from the runner's own descriptors, and I confirmed against the real `check` source that this is a genuine separation, not a fictional one.
- `direct` still cannot pass.
- No `os.killpg`, no `shell=True`, no `rmtree`, no `os.remove`, no `umount`.
  The only signal in the file is `os.kill(pid, sig)` on a single owned pid.
- Case-id traversal is still blocked; ids only ever come from the pin and the classifier.
- The document now discloses what each capability entry is, corrects the `307_recovery` claim, and states the merge-base arithmetic for the comparator diff.
- `verify_source_pin` uses `dirty_paths_all` in preference to the capped `dirty_paths`, so the 20-entry cap I flagged in my second review no longer decides a refusal.

## Protected resources, verified unchanged

Read-only checks only, no signal, no unmount, no deletion.

| resource | state |
|---|---|
| Pi g5 builder daemon 899604 | alive, `cowfs serve --st...`, untouched |
| Pi g4 builder daemon 1209860 | alive, `cowfs-daemon -`, untouched |
| Pi g5 mount | `fuse.cowfs cowfs 0:143`, present |
| Pi g5 control socket | present, mode `srw-------`, mtime Oct 4 16:15 |
| Mac shared daemon 15263 | alive, started Sat Oct 3 20:44:29 2026, untouched |
| all 32 held leases | untouched, no lease operation of any kind |

## What this review did not do

- No commit, push, merge, lease return, reset, stash, or checkout.
  The lease is still at `6a8075a`; the reviewed head was extracted read-only with `git archive` and every file verified against the commit blob by sha256.
- No edit to any source file.
- No SSH write. Every Pi command was a read: `git rev-parse`, `git status`, `grep`, `find`, `ls`, `findmnt`, `/proc` reads, and one bounded Python walk of the reference tree.
- No install, sudo, sysctl, container, capability change, device format, or shared partition.
- No real xfstests batch. The five prerequisites are absent and unauthorised, so the block stands on its own.
- No workflow dispatch, rerun, runner change, or CI polling.
- No signal sent to any borrowed process, and no group kill performed as a negative control.
- No traversal or `stat` of a dead mount; `realpath` was used only on ordinary files under a temp directory.
- No claim about power loss, reclaim, quiet performance, crash behaviour, g6, or macOS.
- No claim that cowfs is or is not xfstests conformant.

## g5 status

**UNMEASURABLE. Zero actual xfstests assertions. OPEN. Issue #101 open.**

Verified read-only on the Pi at the pinned tree:

```
ref/xfstests HEAD:  3e1ee800e52a0f53d7d9a7809be1ffc80ec1788f   (matches the shipped pin)
git status:         clean
ltp/fsstress:       absent
ltp/fsx:            absent
tests/generic/group.list: absent
include/config.h:   absent
include/builddefs:  absent
```

The builder's own final preflight recorded `UNMEASURABLE` with blocking `['ltp/fsstress', 'ltp/fsx', 'tests/generic/group.list']`, pin ok, probe `generic/010 rc=1 outcome=SKIPPED runner=check`.
`group.list` remains a genuine third prerequisite: without it `check` resolves nothing and still prints a summary.

The six reviewed ids remain six of 802, and 755 is documented as unable to fail by its own logic.
Even with every prerequisite lifted, this subset cannot carry the gate.

## Evidence

| file | what |
|---|---|
| `docs/reviews/xfstests-g5-final.md` | first review at `6a8075a`, preserved, sha256 `8d5c171a8c5ca4d68b7d6dfc36da6034d7affe769b130353f88bf35d923e1cda` |
| `docs/reviews/xfstests-g5-repair-final.md` | second review at `b005753`, preserved, sha256 `1076ddb5afdd8253901b8cb9abfe293fd3e6436639fcab812cf646ef00ff5f32` |
| `docs/reviews/xfstests-g5-final-repair-review.md` | this document |
| `bench/out/xfstests-g5-final-critic/tree-445286b/` | the reviewed head, extracted read-only, blob-verified |
| `bench/out/xfstests-g5-final-critic/final_controls.py` | F1 registry, F2 closure, F3 forge and F4 pin controls |
| `bench/out/xfstests-g5-final-critic/controls.log` | their output |
| `bench/out/xfstests-g5-final-critic/realcheck_stream.py` | the real-routing forge harness, including the genuine-failure bypass attempts |
| `bench/out/xfstests-g5-final-critic/realcheck.log` | its output, zero bypasses |
| `bench/out/xfstests-g5-final-critic/unittest-445.log` | 162 tests, exit 0, 1 skip |
| `bench/out/xfstests-g5-final-critic/ruff-445.txt` | scoped F,E9, exit 0 |
| Pi, read-only | pinned tree sha and clean state, the five absent prerequisites, the real sourcing lines, the 16-file closure per case, `findmnt` for the g5 mount, `/proc` for 899604 and 1209860 |