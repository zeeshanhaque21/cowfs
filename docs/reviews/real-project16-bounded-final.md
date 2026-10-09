# Review: PR #114 bounded delta, `c77461d` against `4a70c53`

Reviewer verdict: **REQUEST CHANGES, narrow.**
All seven findings from the previous round are repaired, and the two blockers are closed by real CI, not by assertion.
What remains is one wrong number in the table this review exists to police, three stale cross-references, and one internal inconsistency between two figures in the same document.
Nothing about the acceptance changed: it is still **NOT met**, and #15, #16 and #123 all stay open.

- head reviewed: `c77461d844ae50eb1775e40132d364cf912fd717`
- prior reviewed head: `4a70c53ae4c7062458ae7e63dbe8619cce1458a1`
- code commit: `a64e1189ff5f8ac8d34b24aad5d820818f8eaf72`
- `main` at review time: `951045fca4823611e196eda75db0c977a46d2c77` (PR #96 merged; #111 also in since the branch forked)
- no source edited, no commit, push, merge or lease return; lease stayed at `6df8b9fcfd6f9d54a6af0c8e424f3851fc6ecc71`

## 0. Docs-only carry, pinned links, mirror

```
                                        real_project_acceptance.rs   ready-real-project.md   evidence/real-project16-repair.md
a64e1189  code                          7d7b0135adf269349967af7f15f9710da77c8cec   4aed7a2024b0025ef26c8d9f5e55372ec0a2d013   cc7044414c62c7fc844696da76ed14ac65ae5414
c77461d8  head                          7d7b0135adf269349967af7f15f9710da77c8cec   1e8cea87da03599d1dcbfb858315d569d4493db8   cc7044414c62c7fc844696da76ed14ac65ae5414
```

The test file is byte-identical at `a64e118` and `c77461d`, so `c77461d` carries two documentation lines and no code.
Any measurement at `c77461d` is a measurement of `a64e118` code, which is what the report's own binding table says.
Fetched and hashed: `c77461d:…/real_project_acceptance.rs` is sha256 `8023ddb4f37a66e57495b0d5972a1f6cc490971ca6dceb09c29b1d6681fd0e22`, matching `git show`.

Canonical mirror, verified in the primary checkout rather than assumed:

```
docs/verification/ready-real-project.md             sha256 c84f0cf2a39a302d5baf5b62fba255aebdf9c2e2f1b4ce0b409af3bbfbe98933
  on-disk git blob 1e8cea87da03599d1dcbfb858315d569d4493db8  == the blob at c77461d, exact
docs/verification/evidence/real-project16-repair.md  sha256 fbde334aad6be36992c4350ff291e5a460b9b3f00b9dd4824803e3bdb144a399
  on-disk git blob cc7044414c62c7fc844696da76ed14ac65ae5414  == the blob at a64e118, exact, and identical at c77461d
```

The evidence file is now committed on the branch, and the pinned link `…/blob/a64e1189ff5f…/docs/verification/evidence/real-project16-repair.md` resolves to exactly that blob. No fabricated link, and both documents are committed rather than disk-only, which was the gap last round.

Closing references: none. `closingIssuesReferences` is empty; #15, #16 and #123 are all `OPEN`; #123 is "Define and implement tree-native Core warm-base publication for the companion".
A lexical sweep of all six commit subjects and bodies finds one `for #15/#16` direction phrase and one `fix` inside a negated sentence ("a provenance fix alone cannot make mode (b) publish anything"). No closing keyword, no negated close trigger, no "closes"/"fixes #N" anywhere.
Closing state has to be re-checked at merge time rather than inherited from this snapshot.

## 1. N1 to N7, independently retested

Every control below was run here, against the binary built from this branch, with receipts written into my own path.
Controls first, no mount and no daemon. Nothing recursive was deleted, no dead mount was walked, no kernel wedge was risked.

| prior finding | verdict | what proved it |
| --- | --- | --- |
| N1 mount grammar platform-shaped, ubuntu red | **PASS** | 16-case control green, and `check (ubuntu-latest)` now **success** at `c77461d` |
| N2 drain join unbounded | **PASS**, one scope note | 4-shape control, per-command 3s bound, incomplete drain classified |
| N3 kernel start time discarded | **PASS** | start time registered and compared; recycled-pid control green |
| N4 vacuous `ends_with("")` store check | **PASS** | `Result` with every failure path an error, exact canonical equality |
| N5 warm-claim guard unreachable | **PASS** | string claim now **101**, was 0; typed writer proven |
| N6 rows without an outcome | **PASS** | every row typed; seeded undescribed row **101** |
| N7 stranded daemon undisclosed | **PASS** | disclosed with the receipt preserved byte for byte |

### N1, and the blocker is closed by CI

`parse_mount_line` now decodes both grammars explicitly: a literal `type` keyword followed by the filesystem type on Linux, an option list opening with the type on macOS, octal escapes decoded on source, mount point and type, and a refusal for every other shape, including `src on /point type (rw)`, where an option group stands where a name belongs.
The accepted list is exact per platform, `nfs` on macOS and `fuse`, `fuse.cowfs`, `cowfs` on Linux, never a prefix, and the control additionally asserts the list can never contain the word `type`.
Escape decoding, exact target match and refusal of four malformed shapes are all in the 16 cases.

My run: `mount-grammar-control`, `cases: 16`, `accepted: "nfs"`, exit 0.

The decisive evidence is CI, read once, not polled, nothing dispatched or rerun:

```
run 37261350192  head c77461d8   linux-fuse success | check (ubuntu-latest) SUCCESS | check (macos-latest) in_progress
```

`check (ubuntu-latest)` ran all four steps and succeeded:
`cargo fmt --all --check` with zero `Diff in` lines, `cargo clippy --workspace --all-targets -- -D warnings` with zero warning or error lines anywhere in the log, `cargo test --workspace`, and the bench unit tests, `Ran 170 tests in 27.649s`.
Inside it, the harness itself ran on Linux with the FUSE adapter:

```
running 15 tests
test a_real_project_builds_and_tests_inside_an_exported_slot_snapshot ... ok
test native_control_builds_and_tests_the_sample_project ... ok
...
test result: ok. 14 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 126.35s
```

That is the exact test which failed at `4a70c53` with `the slot is on type not fuse … fstype: "type"`, now green on the same runner.
The repair's own gate is what found the defect and what now clears it.

`check (macos-latest)` was still in progress at the single read and I did not poll it. Nothing is claimed about that job.

### N2, bounded, with the scope stated

`run_bounded_detailed` computes `bound = deadline.remaining().min(cap)` and then one `hard_end = started + bound` that covers the spawn, the child **and** the collection of its output.
That is a per-command bound, not a global test budget: the control passes a 3s cap and the receipt records `bound_ms: "3000"`.
Phase 1 bounds the child and kills only `child.kill()`, its own handle. Phase 2 bounds the drain with `recv_timeout` against the same `hard_end`.
An unreaped pipe is `RunFailure::DrainTimedOut { status, stdout, stderr, waiting_on, bound_ms }`, which carries the direct child's real exit status and the partial output and is never returned as success.
There is no second `wait()`: the status comes from `try_wait`, so there is no second wait to return an empty blob.

My run, from the control's own receipt:
`cases: 4`, `bound_ms: 3000`, chatty `4,194,304` bytes in `72`ms, hung `3004`ms, grandchild `3003`ms, `helper_cleaned: "true"`.
The documents quote 46ms, 3001ms and 3005ms from their run; mine are the same shapes within ordinary variation.

Independent corroboration of the old number: my standalone reproduction of the previous `run_bounded`, compiled with `rustc` alone last round, returned at **12.017s against a 3s bound** when a grandchild held the pipe, and its chatty and hung cases behaved as this repair now behaves.
The document's 20,012ms is the same mechanism with a 20s helper.

Scope note, stated rather than buried: the two reader threads per call are detached and never reclaimed, so a descendant that never releases the pipe keeps them for the life of the test binary.
The phase is bounded and correctly classified, so nothing hangs, but the count is bounded by the number of such calls and not by reclamation.
The only producer I can identify is the control's own `sleep` helper, which the control records about itself, verifies is exactly `sleep 20`, cleans, and asserts; I observed `helper_cleaned: true`.
The document says the same about the control's helper, including that an earlier revision orphaned one and that the orphan was verified gone.

### N3, N4, N5, N6, each with the negative control I ran

N3: `Registered` now carries the kernel `start` string and `Live` carries it too; `identity_matches` requires `got.start == want.start` **and** argv containing store, socket and exe.
Identity is re-read immediately before the only `child.kill()` in teardown.
`SHARED_DAEMON_PID: u32 = 15263` is a named constant and the bounded-runner control asserts no signalled pid is it.
Control `a_recycled_pid_with_the_same_argv_is_refused`, `cases: 2`: same argv with a different start refused, same start with same argv accepted.

N4: `status_store` returns `Result<String, String>` and errors on a missing CLI binary, a non-zero exit, non-JSON output, an absent `store_path` and an empty `store_path`.
Both call sites do `assert_eq!(answered, core.owned_store())` with `owned_store()` canonicalised, so unanswered is a hard failure and the comparison is exact equality with no suffix, prefix or basename arm.

N5: the writer is typed (`Field` with its own `Serialize`), so a receipt carries real JSON booleans; the control's own receipt shows `warm_base_published: false` unquoted.
Negative controls I ran: seeded string `"true"` → **101** (last round it was 0), seeded boolean `true` → **101**.

N6: every row carries an outcome. `acceptance.jsonl` verified by parsing: **28 rows, 24 `measured`, 4 `cleanup`, 0 without**.
Seeded row with no `outcome` → **101**, with the message naming the offending row.
The four `cleanup` rows are teardown receipts and are not test executions; the executions are counted from libtest and from `--list`, not from rows.

Also verified, not assumed: missing sibling binary in an empty directory → **101**, "a missing binary is never a pass".

### N7, disclosed to the standard I asked for

The pre-fix receipt is preserved and every number in the disclosure checks out: sha256 `0309db70e65a46f3ab4da572f663732cd948726272ddcb1299dd77aa86666a5b`, 15,243 bytes, 29 rows, and the pid 2875 row exactly as quoted.
The documents state that the harness behaved correctly, that the failure was disclosure and diagnosis, that the cause of the spent deadline is **UNKNOWN** and that the unbounded join is only a **hypothesis, not a finding**, that the cleanup was done by hand from a shell with **no harness receipt** and none invented, that each step of it is **UNVERIFIED by any receipt this lane produced**, and that **the pid being gone now proves nothing**.
They also explain that the file is a hand-assembled mixture whose name understates it, and that the rows are preserved rather than rewritten.
That is exactly right, and "absence alone is no ownership proof" is now written into the document instead of left for the reader.

## 2. My own evidence

Controls above, plus one small complete real Core chain at this head's sample source, under one 600-second foreground lock.
The 263-second suite was not repeated.

Sample: `git archive c77461d` of `crates/cowfs-treehouse`, 18 files, 360,928 bytes.
Binaries: the same three artifacts as both prior reviews, labelled **UNVERIFIED as to build provenance**:

```
cowfs-daemon    b6b9970944b487249d3041f40ed78d6c8755116e3df19b7d7d953cf14b4eeb51
cowfs           59b88405ab1e3ad00ed242938684ae01f56fe2254bfa18e34a6fe7c445bb9f3c
cowfs-treehouse 65a42acb158c37e46b4a454057cd0d56bec7d90d8c7c5ef51dc4f1f0b15fab6e
```

Artifact identity only. Nothing binds them to a recorded source build, they say which bytes answered, and this branch changes no production source.
`main` is now `951045f`, two merges past the branch base, with production NFS code changed underneath; nothing in this PR speaks to merged `main` and nothing in my run does either.

**21 assertions, 21 true, 0 failed, 0 capability skips, 47 flushed rows.**

```
adapter                         nfs, decoded by the daemon and matched against the native table
import                          exit 0, verified true, 18 files, 360,928 bytes
source_root_hash                d5eab6ba83e41c08bf14004a454b95adfcaf95ce0d178359b046012f57ca4ee9
imported_root_hash              d5eab6ba83e41c08bf14004a454b95adfcaf95ce0d178359b046012f57ca4ee9
answering store                 the private store this run opened, exact
fork parent                     rep-base
symmetric readback              source 23, export 23, only-in-source 0, only-in-export 0
export and native tree digest   cc2e911b4ec7c7025b42c2a5b5750b5177246cd93ac53d55761f2a99aa712199
write into the export           round trip exact, base Cargo.toml unchanged, marker absent from base
reset                           exit 0
live readback 0.01s             24 entries, digest differs, marker already gone
live readback 2.02s             23 entries, equals base and equals the native source
fresh daemon, same store        23 entries, equals base and native source, marker gone
teardown, both daemons          gone true, signalled false, mounts_left_listed []
shared daemon 15263             argv byte-identical before and after
```

The lag is recorded at both timestamps and is not waited on and then asserted, and the fresh daemon confirms the reset is durable in the store.

Receipt bindings verified by parsing, not by prose: `acceptance.jsonl` carries `workspace_head a64e1189` four times and `sample_commit a64e1189` seven times, with 2573 source, 2573 export, 2573 base, 2573 slot after reset, 0 only-in-export, 0 only-in-source, `files 2194`, `bound_ms 3000`, `grandchild_elapsed_ms 3001`, `helper_cleaned true`.
That is the representative run the report cites, and the expensive in-slot and native builds are correctly attributed to `4a70c53` as a distinct, historical set.

## 3. What is still wrong

### M1, must fix: the executed count is understated by one

```
--list on the built binary                    15 tests, 0 benchmarks
check (ubuntu-latest) at c77461d              14 passed; 0 failed; 1 ignored
both documents                               default tests in the file 15 / executed and asserted for real 13
```

15 tests, 1 ignored, therefore **14 executed**. Both the report and the evidence file say 13.
The label is wrong too: 15 is the total including the ignored acceptance, not "default tests".
And the six safe controls are listed on their own line without saying they are a subset of those 14 rather than additional tests, which is how the two numbers drift apart.
This is the one table the previous two rounds of this review existed to police, and it is off by one in both documents.

### M2, should fix: two documents count two different receipt files without naming either

The report says the receipt file for this head is "written only by the run recorded here: 28 rows, of which 24 carry `outcome: \"measured\"` and 4 carry `outcome: \"cleanup\"`".
The evidence file says "the receipt file is append-only across several runs, so its 42 rows are not a count of tests; 37 carry `outcome: \"measured\"`, 5 carry `outcome: \"cleanup\"`".
Both are true of different files, and I verified both: `acceptance.jsonl` is 28/24/4/0 and `acceptance-a64e118-pre.jsonl` is 42/37/5/0.
Read together they look like a contradiction. Naming the file in each sentence removes it entirely.

### M3, should fix: the same document quotes two different reset sizes

Line 114 of the report says reset returned "2508 both sides".
Its own reset section, and the receipts for the run it cites, say 2573: source 2573, export 2573, base 2573, slot after reset 2573.
2508 is the pre-commit run in the other file.
One figure is stale in the summary table while the correct one is in the body.

### M4, should fix: #123 exists and is not cited

The report's ownership table gives the blocking seam as "needs an issue that scopes it".
Issue #123 now scopes it: "Define and implement tree-native Core warm-base publication for the companion", open.
The document is stale on the single fact it most needs to be current about, and with no closing reference nothing links the two automatically.
The rest of the chain reads correctly and stays separated: `can_ingest` refusal first and intentional for the directory-import model rather than a new Core bug, then #97 with repair `b59bc3c` still unmerged, then the companion's missing `mount_snapshot` call, then #98 provenance reachable only once something publishes.

### M5, informational: opening sentence counts three, the chain has four

"three further defects sit behind it", then a chain of four links. Cosmetic, but it is the kind of drift a reader uses to decide what to trust.

### M6, informational: per-test timings exist in one document only

The evidence file carries 14.7s, 151.9s, 94.1s, 4.8s, 6.0s, 4.8s, 1.5s and 0.1s per test; the report carries none and says "no timing is claimed".
That is defensible, because they are different documents and the expensive two are labelled historical at `4a70c53`.
I did not re-measure any of them and did not repeat the suite.
No figure here is a performance claim, and the build-overhead criterion stays open.

## 4. Required before merge

1. Correct the executed count to 14 and the label to 15 total with 1 ignored, and say the six controls are a subset. M1.
2. Name the receipt file in both documents' counts. M2.
3. Make the reset size in the summary table match the run the document cites. M3.
4. Cite #123 for the publication seam. M4.

None of these touches code, and none changes a gate.
With them the delta is mergeable as the honest negative result it is: acceptance NOT met, chain correctly ordered, no false green anywhere I could reach, and every safety claim now backed by a negative control that fails when it should.

## 5. Scope not crossed

- Source read-only. No checkout, reset, stash, commit, push, rebase, merge or lease return.
- Writes confined to `bench/out/real-project-bounded-final-critic/**` plus this one assigned report.
- No production source touched: not the `can_ingest` boundary, not `import.rs`, not #98, #20, #79, #92, #96, #100, #102, #106, #112, #113, #116, #118, #119, not the companion.
- #123 read only; no production fix requested or duplicated. The unfixed `import.rs` is still blob `df2c831d` at `46b0f26`.
- No 263-second batch, no repeated 600-second wait, no Linux cross-build, no 805M soak, one foreground lock acquisition for mounted work and none for the controls.
- CI read once per run, one job log per head. No dispatch, rerun, poll, runner config or workflow change.
- Every 32 leases untouched; shared daemon 15263 and its store, mount and socket never signalled, unmounted or traversed, re-verified after the run; the g4 and g5 fixtures not inspected or touched.
- No install, sudo, sysctl, reboot or device operation.
- No warm-benefit, performance, dedup, last-writer-wins or crash-durability claim is made or accepted.
- Raw evidence stays private to the lanes. No artifact link in this report that does not resolve.

## 6. Verdict

**REQUEST CHANGES, narrow.**
All seven previous findings pass, the two blockers are closed by a real green `check (ubuntu-latest)` on this exact head, and the acceptance is still not met with #15, #16 and #123 open and nothing closing them.
The residual is bookkeeping: one count off by one, two figures that belong to different runs, one receipt file counted twice under two names, and one issue that now exists and is not cited.