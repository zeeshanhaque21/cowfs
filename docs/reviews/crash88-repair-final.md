# Repair review of PR 91, critic 12

Reviewed head `038556b244796f5787def6db7559308f2c1fb1c4`, base `ceb96c67033cbf97f79267d6af7db3fa204d77d1`, branch `review/full-stack-crash-88`.
This is a follow-up to my first review of `94998b2c5cb9eb878f08cf38fdd706d03f027538`, which is preserved unchanged at `docs/reviews/crash88-final.md`.
All raw fixtures and probes from that review are preserved: `bench/out/crash88/critic12-*`, `bench/out/crash88-critic/**`.
New artifacts are `bench/out/crash88/r12-*` and `bench/out/crash88-repair-critic/**`, both gitignored.
No source, test or CI file was edited. `git status --porcelain` shows only `docs/reviews/crash88-final.md` (carried over) and `docs/reviews/crash88-repair-final.md` (this file).

The three commits under review are `31fcaf8` (remove the downgrade, assert the POSIX boundary, fail closed on resume), `d071ce2` (match the known-failing label against the whole case name) and `038556b` (report the blocked boundary).

## Verdicts

| scope | verdict |
|---|---|
| the instrument: harness, source tree unchanged, proof discipline | **PASS with two defects and one CI break** |
| success criterion 3 and gate g6, for `rename` + `fsync` | **BLOCK, unfixed, and now correctly reported as blocked** |

My five blocks from the first review are addressed. Details, each with what I actually ran.

## The five blocks, and what I measured

### 1. Unconditional B to A demotion with a hidden event: fixed

`Receipts` has no `downgrade` method at all now. I confirmed by grep and by loading the module: `hasattr(Receipts, "downgrade")` is `False`.
`Receipt` uses `__slots__`, so no attribute can be added to an instance either.
There is no `not_durable` boundary prefix anywhere in the code.

In my full run (`r12-full`, `--stage all --reps 2`, 29 executions) I walked every per-case ledger and searched every record for the strings `downgrade` and `not_durable`: **0 occurrences**, across 72 `receipt.issued` records (52 durable, 18 applied, 2 removed).
Receipts are issued with an explicit `meaning` and persisted immediately:

```
receipt.issued {"boundary":"nfs_commit","kind":"durable","path":"live/orig.bin",
                "seq":1,"sha256":"3abf7c6a...","size":4096}
```

Every receipt in the failing cases stays `durable`. `rename_posix_durability-sample-r0` ends with `outcome=fail`, receipt `durable:live/moved.bin`, failure `durable_present`, and the ledger shows it was issued as `durable:live/orig.bin` and never re-labelled.

### 2. The rename cases now fail for real: confirmed

`case_rename_posix_durability` asserts POSIX durability and has no measurement gate and no expected-failure wrapper. `KNOWN_FAILING` is used only to annotate the human summary; `main` still exits 1 on those failures.

My runs:

| run | result |
|---|---|
| `r12-sample` (`--stage sample`, 1 rep) | exit 1, 2 executed, 1 passed, 1 failed |
| `r12-rename3` (3 rename cases x 3 reps) | exit 1, 9 executed, **3 passed, 6 failed**; `rename_posix_durability` lost 3/3, `rename_posix_durability_ro` lost 3/3, `rename_committed` survived 3/3 |
| `r12-full` (`--stage all --reps 2`) | exit 1, **29 executed, 23 passed, 6 failed**, matching the report's claim exactly |

The failure record carries the sibling listing, so loss of a name is distinguishable from the file vanishing:

```
assert.durable_present ok=false path=live/moved.bin parent_entries=["orig.bin"]
```

`fsck` is clean in all 6 failing executions, so this is loss of an uncommitted name, not corruption. Bytes are intact under whichever name survived.

### 3. Accounting: fixed, 29 planned and 29 terminal

`case_identity` is a digest over `schema, phase, case, rep, rev, harness_sha256, daemon_sha256, cli_sha256, config, argv_scope`. Phase is in the key, so the old collision is gone.

Measured on `r12-full`: 28 `case.begin` records in the parent ledger (the native control writes its own terminal record but no parent `case.begin`, which is a cosmetic asymmetry, not a count error), 28 distinct identity keys, **0 collisions**, 28 distinct case directories.
`write_fsync` was planned four times as `sample/r0, sample/r1, matrix/r0, matrix/r1` and got four separate directories.
Totals are now taken from `case.terminal` records: 29 terminal, 23 `pass`, 6 `fail`, and `summary.json` reports the same. 52 durable receipts reconcile exactly: 46 `assert.durable_match` plus 6 `assert.durable_present` failures equals 52.

### 4. Resume: mostly fixed, one residual forge surface

Fixed and verified by my own independent controls (`forge_lab.py`, 41 checks, 39 pass, 2 fail, both failures being harness gaps I report below or controls I rewrote):

- A one-line `{"terminal": true, "key": ...}` stub is rejected: `terminal record identity mismatch`.
- A stub in a fresh run directory forces re-execution of both cases (`executed / reused : 2 / 0`) and the genuinely failing rename case still fails the run with exit 1.
- Foreign `rev`, zeroed `daemon_sha256`, zeroed `harness_sha256`, wrong `config`, wrong `argv_scope`, missing `assertions`, `outcome: aborted`, deleted evidence file, stripped `receipt.issued` lines, changed receipt sha256, removed `assert.*` lines, removed terminal record, and a self-consistent plant of a whole foreign identity are each rejected with a specific reason.
- A torn final line is tolerated and the cache still validates.
- A cross-revision record is reusable only under its own key, never under another run's key.
- A cached-only run exits **2**, prints `verdict: cached_only`, `fresh acceptance: NO - zero cases executed`, and writes `fresh_acceptance: false` into `summary.json`. With `--accept-cached` it exits 0 and still prints and writes `fresh_acceptance: false`. Both surfaces refuse the claim.
- A cached **failing** case still fails: exit 1, never `UNMEASURABLE`, never `executed_all_passed`.

Two residual gaps, both real:

**(a) A failing verdict can be laundered into a cached pass.** I took a real `outcome: fail` manifest from the rename case, set `outcome` to `pass`, emptied `failures`, set every manifest assertion to `ok: true`, aligned the manifest receipts to the ledger paths, rewrote the embedded terminal manifest, and flipped the ledger's assertion records to `ok: true`. `validate_cached` **accepted** it and would report `outcome: pass` on reuse. The same holds when I leave the failing assertion at `ok: false` in the ledger: the check is on the assertion **label** being present, not on its value. `validate_cached` compares receipts on `path` + `sha256` only, so `kind` is never compared either: weakening every `durable` receipt in the ledger to `applied`, or changing `kind` in the manifest, is accepted.

What this means in practice: the earlier repair's *hardening* is sound (a one-line stub, a foreign identity, a stripped receipt, a wrong digest all fail), but `manifest.outcome` is still self-reported and the cross-check against evidence is name-based rather than value-based. The identity tuple is a consistency check, not authentication, and it cannot be an authentication check because anyone who can write `manifest.json` can also recompute the key. A cached verdict is only as trustworthy as the directory it sits in.

I did **not** find a way to turn this into a false pass of a *fresh* run, and I tried hard: the laundered directory I planted under `r12-launder` was rejected by the CLI on the very next run because the identity differed from what that invocation asked for, and the real run still exited 1 on the real failure. And my end-to-end attempt against `r12-sample` was caught by the `repath` mismatch described in (c), which rejects every attempt directory for that case. So the practical exposure is "someone edits a cached verdict by hand and the next run of that exact identity reuses it", which is a deliberate-tamper scenario rather than an accidental one. It still should be closed, and the fix is small: store the outcome and the assertion values in the ledger, and require the manifest to match them exactly.

**(b) `nfsstat` availability is not portable, and CI is red because of it.** PR 91's `check (ubuntu-latest)` job **fails**: `FAILED (failures=1)`, `Ran 86 tests`, the failing test being `TestProbeLabelsMustMatch.test_sampled_and_marked_produce_a_delta_or_an_explicit_error`. `macos-latest` and `linux-fuse` pass. I reproduced the cause locally by pointing `NFSSTAT` at a nonexistent path: `Probe.report` then returns `commit_delta: None` with a non-null `error` and `available: False`, and that test asserts `commit_delta` is not `None` and `error` is `None`. So the test encodes a macOS-only assumption about a tool that is macOS-only. The harness itself handles absence correctly and honestly (`available: false`, explicit `error`, `attributable: null`); it is the test that is wrong. Minimal fix: have that test skip or assert the documented unavailable branch when `probe.available` is `False`, or inject a fake counter reader.

This is a merge blocker in the ordinary sense: the PR cannot land with a red required check, and I am not permitted to dispatch a workflow to confirm anything else.

### 5. No CI coverage of the harness: fixed

`bench/test_daemon_crash.py` is new, 588 lines, 50 tests. It is discovered by the step CI already runs, `python3 -m unittest discover -s bench`, with no workflow change.
Locally: `python3 -m unittest bench.test_daemon_crash` runs 50 tests, OK. `python3 -m unittest discover -s bench` runs **86 tests, OK** (36 pre-existing plus the 50 new).
Note for whoever merges: other unmerged benchmark and namespace work in the pool adds 51 and 17 tests respectively that are not in this tree, so the 86 here is this tree's total, not the fleet's.

The tests use synthetic ledgers, fake fixtures and short-lived private processes: no daemon, no mount, no cargo, nothing under `~/.cowfs`. That is what I want from CI-runnable controls.

## Native control: now does the same operations

This was my item 5 from the first review and it is fixed. The native writer performs the rename and the same syncs, and the case passes:

```
native.spawned mode=kill
assert.native_writer_exit_kill ok=true
assert.native_durable_match_kill ok=true path=snap/moved.bin want=bbc5d95f... got=bbc5d95f...
native.spawned mode=clean
assert.native_writer_exit_clean ok=true
assert.native_durable_match_clean ok=true path=snap/moved.bin
manifest outcome=pass assertions=6 failures=[]
```

`--stage native` on `r12-native`: exit 0, 1 executed, 1 passed, 0 failed.
So the divergence is now demonstrated against the same operation model on both sides: native APFS keeps the renamed name through a writer `SIGKILL` after a parent-directory fsync, cowfs does not.

## Wire measurement: honest about its own limits, with one bug

The claim is no longer inferred from a lost rename. `Probe` reads the kernel NFSv3 client `Commit` counter with `nfsstat -c`, which needs no root, and the parser pins the NFSv3 section because `nfsstat` also prints an NLM section with its own `Commit` column. I verified the parser against a two-section sample: it returns the NFSv3 value 7 and never the NLM value 999. My own first-review measurement stands, with the same three-row shape.

The report states plainly that the counters are host-wide, gives a drift range, and marks unattributable reports. That is the right disclosure and it is more careful than my first review, which used the counter without that caveat.

**The `attributable` flag has an operator-precedence bug**, `scripts/verify-daemon-crash.py:593`:

```python
attributable = drift == 0 and delta > 0 or abs(delta) >= 3 * abs(drift)
```

Python parses this as `(drift == 0 and delta > 0) or (abs(delta) >= 3 * abs(drift))`. With `drift == 0` the second clause is `abs(delta) >= 0`, which is always true. Measured truth table:

| delta | drift | attributable reported |
|---|---|---|
| 0 | 0 | true |
| 2 | 0 | true |
| -1 | 0 | true |
| 0 | 14 | false |
| 0 | 2 | false |

So on a quiet host `attributable` is `true` for every step, including steps with a zero delta, which is precisely the case the flag is supposed to distinguish. In my `r12-all1` run every idle drift was 0 and all four wire reports came back `attributable: true`, two of them with `commit_delta: 0`. The report's own table, from a busier host, shows `attributable` correctly going false when drift was nonzero, which is why the bug survived their run. Minimal fix: parenthesise, `attributable = (drift == 0 and delta > 0) or (drift > 0 and abs(delta) >= 3 * drift)`.

This does not change the verdict. The crash-side measurement is what the verdict rests on, and the report says so. It changes how many of the wire rows can be believed.

## Provenance

Accurate now. The report cites `git rev d071ce251e57021cd63ba0866f91731e92a77a3e`, the commit that last touched the harness, with `harness digest 30270e72e47b571652a495266371eb26793b535417ca33866b068be911126378`.
I re-hashed the file at head and got that digest exactly.
It states that the harness does not exist at base `ceb96c6` and that the earlier revision's citation of `ceb96c6` was wrong, which is the correction I asked for.

One nuance worth being precise about: `d071ce2` is the parent of head `038556b`, not head itself. The run identity records `rev: 038556b244796f5787def6db7559308f2c1fb1c4` in my `summary.json`, which is correct for a run I performed at head. Since `038556b` only touches the report, the harness digest is identical at both revisions, so no verdict depends on the distinction. Stating the head sha would be tidier.

## Accounting and supersession

The report keeps the superseded run's numbers and refuses them: 856 records, 0 failing, 48 of 48 durable receipts matched, and the reason it is excluded, namely that 4 of its 52 durable receipts had been reclassified by an unmeasured `Receipts.downgrade()` that was never written to the ledger.
That is an accurate description of what I found, and it is the correct call.
The gc case still reclaims nothing (158 and 163 candidate blocks, `freed_bytes` 0) and the report still says so and points at `docs/verification/gc-daemon-e2e.md` for real reclamation.

## Safety, re-verified on this revision

- Every signal went to a pid this run spawned, gated on the command line carrying this run's `--store` and `--socket` **and** on the pid's start time matching the identity captured at spawn. I confirmed the gate fires on each field independently: mismatched `pid`, mismatched `lstart` and mismatched `cmdline` each raise `ForeignProcess`, and the offered `sleep` pid is alive after each refusal.
- `selftest.kill_refuses_foreign_pid` now also records `still_alive_after_300ms`, the probe I had to add by hand last time. It passes.
- `unmount_private` only unmounts a path the mount table lists exactly; every refusal is recorded.
- The daemon under test starts with `start_new_session=True`, so a tool timeout cannot reap it.
- Crash-side assertions still fail closed. I re-derived the controls against the new `CaseResult` objects: missing file, wrong hash, wrong size and a reported fsck problem each produce `ok=false` assertions (`durable_present`, `durable_match`, `fsck_clean`), and only `applied` items' absence is tolerated.
- Shared state untouched: pid 15263 is still `Sat Oct 3 20:44:29 2026` and is the only `cowfs-daemon` running, the only cowfs mount is the shared `~/.cowfs/mnt`, and no `cowfs-crash88-*` socket directory leaked. No signal, scan, collect, GC, restart, dispatch or rerun was sent to anything.
- Budgets are declared before the run: 64 KiB per op, 16 ops per case, 180 s per case, 90 s without progress. My full run took 90 s of wall clock for 29 executions.

## Still unknown, and not a pass

Unchanged by this repair, and the report still lists them correctly:

- power loss; `SIGKILL` cannot exercise it, and every un-fsynced write surviving here is a consequence of the host page cache, not of durability.
- mid-`gc` crash, and the two internal orderings between pack fsync, watermark advance and metadata commit.
- concurrent writers, and any writer that is not this harness.
- reclamation; the gc case frees 0 bytes by design.
- `shutdown` as the crash boundary.

## Issue 90, and what still must happen

The report's closing section is right and I endorse it: the source fix is not in this branch, issue 90 must carry either a durability barrier a caller can reach or a narrowed capability claim in `docs/design.md`, and `rename_posix_durability` and `rename_posix_durability_ro` must then pass on the fixed tree with no wrapper.

I checked `cowfs_ctl::Request` (`crates/cowfs-ctl/src/types.rs:432`) again at this revision: still no `sync` request, so the control plane offers no barrier either. A `cowfs-ctl sync` alone would not close the boundary, because the failing boundary is the ordinary POSIX `fsync` path, not the control plane. Any fix has to make a namespace-only operation durable when the client's fsync crosses no wire: a server-side write-through of rename/create/rm, a mount option that forces a real `COMMIT`, or a documented capability limit with `docs/design.md` narrowed. I am not implementing any of those; they are not my paths and issue 90 owns the scope.

The builder's edit to issue 90's body proposing a `mount_nfs` option, a barrier, or a narrowed claim is a **proposal**, not an authorised change to the contract. Nothing in this PR narrows the full-POSIX claim, and it should not.

## Minimal fixes, in order

1. Fix the ubuntu test so `check (ubuntu-latest)` is green. Either assert the documented `available: false` branch or inject a fake counter reader. This is a merge blocker.
2. Parenthesise the `attributable` expression at `scripts/verify-daemon-crash.py:593`.
3. Close the laundering surface: persist `outcome` and per-assertion values in the ledger and require the manifest to match them exactly, and compare receipt `kind` as well as `path` and `sha256`. Small, and it makes the "resume fails closed" table in the report true for the case the table does not currently name.

None of the three needs a production change, and none is mine to make.

## What I did not verify

- Whether the builder's original 6-of-8 wire table is reproducible on this host. My runs were on a quieter machine where every idle drift was 0, so I could confirm the flag's behaviour and its bug but not their drift range.
- Power-loss, mid-`gc` and reclamation behaviour: unsampled by construction.
- The `cargo test --workspace` and clippy results. CI reports them as part of the failing job but the failure is the bench test, and I did not run the Rust suite locally.
- Any behaviour on Linux or FUSE beyond reading the CI job results.
- The other unmerged work in the pool (benchmark 51 tests, namespace 17 tests). Not in this tree.

## Bottom line

The five blocks from my first review are genuinely fixed, and the central one is fixed properly: the harness now refuses to pass a boundary it cannot deliver, it says so in its own exit code, and the report leads with a blocked verdict instead of a green one. That is the correct shape for this artifact, and the failing rename cases are worth having even though they are red.

Merge is blocked today by one thing I cannot fix from here: `check (ubuntu-latest)` is red on a macOS-only assumption inside a new test.
Fix that, parenthesise the `attributable` expression, and close the manifest-laundering gap, and I would merge this as the instrument plus an honest blocked finding.
g6 and success criterion 3 stay blocked for the `rename` + `fsync` boundary until issue 90 lands a real fix.