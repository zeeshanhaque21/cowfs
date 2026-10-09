# Review: PR #114 repair delta, `4a70c53` against `1ca8242`

Reviewer verdict: **REQUEST CHANGES.**
Fourteen of the sixteen prior findings are genuinely repaired, and two of them are repaired well enough to catch a real defect in the repair itself.
Two merge blockers remain, both in the new safety code, plus one disclosure gap about a real stranded daemon.

- PR: #114, head `4a70c53ae4c7062458ae7e63dbe8619cce1458a1`, branch `verify/treehouse-real-project-16`
- prior reviewed head: `1ca8242a1a134ae25d43292d00f0a25c852efad6`
- PR base of record: `03bbec85626c26a85ee4f5791d3a413fe47725bc`; `main` has since moved to `3a6935b244d0209ef7c75ddf9539f54dc137105b`
- closing references: none. `closingIssuesReferences` is empty, #15 and #16 both `OPEN`, verified by GraphQL alias.
- No source file edited. No commit, push, merge or lease return. Lease stayed at `6df8b9fcfd6f9d54a6af0c8e424f3851fc6ecc71`.

## 0. The docs-only carry is structural, not asserted

Blob identity, from git, not from prose:

```
                                              real_project_acceptance.rs   ready-real-project.md
1ca8242  (harness)                            85cfbb628b50d0b5276c44de1b059b6e404882c7   b4da55b18e51544af4e92846e0d1816bcdcdc1e1
1b1f2e1  (repair)                             e25e233c624467707a1395dd014564504ea171ec   b4da55b18e51544af4e92846e0d1816bcdcdc1e1
4a70c53  (head)                               e25e233c624467707a1395dd014564504ea171ec   2e43855e735e6eea7fe77dab14f284e4f93cef14
```

The test file is byte-identical at `1b1f2e1` and `4a70c53`, so the runtime under review is the `1b` tree.
Anything measured at `4a70c53` is measuring `1b1f2e1` code; anything measured at `1b1f2e1` is measuring the same code.
The document says this itself: line 9, "Commit these receipts belong to: `1b1f2e1`", and line 15, "Every number below is from one coherent single-threaded run at `1b1f2e1`". Correct framing, and I did not let a `4a` runtime claim stand anywhere.

Fetched and hashed: the test file at `4a70c53` is sha256 `0de2140493d526be595aaf821bd08486072410f0c29bf95506029a14d0f80667`, identical to `git show 4a70c53:…`.

### Canonical mirror

```
docs/verification/ready-real-project.md            sha256 651b301cccee4eec190a3162b65cf3bb099ad46258e59c56e6dc3180868fa408
  git blob 2e43855e735e6eea7fe77dab14f284e4f93cef14  == the blob at 4a70c53, exact
docs/verification/evidence/real-project16-repair.md sha256 bd12cfbbe59f3a0b1851fb9c0918eda30200ee948a5e06dbfd8cc42e054e79b1
  no committed blob exists for the evidence file; it is on disk in the primary checkout only
```

So the ready document is mirrored blob-for-blob with the PR head. The evidence file matches the stated digest and exists, but nothing has committed it.

### Commit-message direction, checked lexically and then for real effect

All three commit subjects and bodies:

```
1ca8242  test(treehouse): real-project acceptance for #15/#16, failing closed on the warm base
1b1f2e1  test(treehouse): make the acceptance harness safe, honest and rustfmt-clean
4a70c53  docs(verification): record this head's numbers, the chain, and the measured limits
```

"for #15/#16" is a direction word, not a closing keyword, and GitHub resolved it as such: zero closing references.
A closing phrase would need to survive a future merge, and nothing here should be read as one.
The final merge must be checked again at that moment rather than trusted from this snapshot.

## 1. Prior findings, one line each, with what proved it

| prior finding | verdict | proof |
| --- | --- | --- |
| F1 missing binary reported `ok` | **PASS** | repaired test binary alone in an empty directory, exit **101**, explicit message |
| F2 fmt red, CI never ran the suite | **PARTIAL** | fmt genuinely fixed, but a new ubuntu failure appeared (blocker N1) |
| F3 blocking dependency misattributed to #98 | **PASS** | ignore reason now names the chain, document names the gate first |
| F4 no binary/source manifest | **PASS** | workspace head, rustc and three digests in every daemon receipt |
| F5 document quoted another commit's numbers | **PASS** | receipts bound to `1b1f2e1`, earlier runs labelled historical in a table |
| F6 in-slot build not proven inside the export | **PASS** | mount identity and answering store read back before the build |
| F7 ignored gate had no reset | **PASS** | reset per slot, tree and manifest equality, base intact after two slots |
| F8 acceptance must stay nonaccepted | **HELD** | still `#[ignore]`d, never executed to a pass, #15 and #16 `OPEN` |
| F9 `Added`/`AlreadyThere` recorded not asserted | **PASS** | both asserted, plus the `post_create` line compared with what treehouse reads |
| F10 export readback one-directional | **PASS** | `only_in_export` and `only_in_source` both asserted, 2399 vs 2399, 0 and 0 |
| F11 `Drop` panics, fails open, unbounded commands | **PARTIAL** | tri-state and bounds fixed, but the joined pipe readers are still unbounded (blocker N2) |
| F12 watchdog abort skipped teardown | **PASS** | zero `Watchdog` references in the file, replaced by per-phase deadlines |
| F13 #97 attributed to a docs commit | **PASS** | `b59bc3c` named with its files, `e243cb1` explicitly rejected |
| F14 materialiser gap overstated | **PASS** | one transition, companion-side, bounded again by my own run |
| F15 post-reset lag undisclosed | **PASS** | recorded as a measured limit at 0s and 2s, not as a coherence guarantee |

Counts, derived independently from the raw source and receipts rather than taken from the document:

```
#[test] attributes in the file                    11
#[ignore] attributes                              1   (the acceptance)
default tests executed and asserted                10
executed as a capability skip                       0
records in the run's acceptance.jsonl              29
records carrying outcome "measured"                24
records with no outcome field                       5   (every teardown record)
author's single-gate smoke                        14.59s
author's full suite                              263.52s   10 passed, 0 failed, 1 ignored
```

The author's numbers check out, including `29 flushed records` and `0 capability skips`.
The one number that does not check out is the schema: 29 records but only 24 outcomes, because no teardown record carries one (finding N6).

## 2. Blockers

### N1, blocker: the new mount-identity check is macOS-shaped and turns ubuntu CI red

The repair added a real readback, and it works on macOS.
It mis-parses Linux `mount` output, so the assertion it feeds fails on `ubuntu-latest` at both `1b1f2e1` and `4a70c53`.

`mount_identity` takes the token after the mount point and strips a leading `(`:

```
macOS   localhost:/cowfs-6288… on /…/p/1/sample (nfs, nodev, nosuid, mounted by …)
        token[1] = "(nfs,"   ->  fstype "nfs"          correct

Linux   cowfs on /…/p/1/sample type fuse (rw,nosuid,nodev,relatime)
        token[1] = "type"    ->  fstype "type"        wrong
```

CI, read once each, not polled, nothing dispatched or rerun:

```
run 37257150516  head 1b1f2e1  linux-fuse success | check (macos-latest) SUCCESS | check (ubuntu-latest) FAILURE
run 37257613629  head 4a70c53  in_progress at read time; linux-fuse success, check (ubuntu-latest) FAILURE, check (macos-latest) in_progress
```

The ubuntu failure, verbatim from the log:

```
thread 'a_real_project_builds_and_tests_inside_an_exported_slot_snapshot' panicked at
  crates/cowfs-treehouse/tests/real_project_acceptance.rs:1643:5:
the slot is on type not fuse: MountIdentity { point: "/tmp/.tmpQUn5hd/th/.treehouse/p/1/sample",
  source: "cowfs", fstype: "type" }
test result: FAILED. 9 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 95.06s
Process completed with exit code 101
```

My own host reproduces the token order directly: on macOS the harness's extraction gives `nfs` and the third token is `nodev,`, which is the option list, not the fstype.
So this is a parser that encodes one platform's grammar.

Two things follow, and they matter more than the red light.
First, F1's repair is proven in CI exactly as intended: under `cargo test --workspace` the workspace binaries exist, the daemon really started, the export really mounted, and a wrong observation became a red test with exit 101 instead of a green marker.
Second, the blocker is in the PR's own test code, so the fix is the PR's to make, and it is a parser change plus a synthetic case for the Linux line shape.
No production patch is requested and none belongs to another lane.

### N2, blocker: the joined pipe readers are unbounded, so the absolute deadline can be overrun

`run_bounded` takes the deadline before the first spawn, drains both pipes on owned threads while the child runs, kills only its own pid at the bound, and joins the readers on the success path with no bound of its own.
A child that exits while a grandchild still holds the pipe open makes that join block indefinitely, past the deadline, and the function returns `Ok`.

Proven by execution, not by reading. `bench/out/real-project-final-critic/bounded_pipe_repro.rs` is a standalone copy of `run_bounded`'s exact structure from lines 157-222, compiled with `rustc` alone into my own path, no workspace target directory, no crates. Log in `bounded_pipe_repro.log`:

```
bound = 3s for every case

case 1  chatty, 4 MiB on stdout      exit_seen 83ms      returned 85ms       4,194,304 bytes   overrun=false
case 2  empty stdout, exits at once  exit_seen 27ms      returned 27ms       0 bytes           overrun=false
case 3  hangs (sleep 30)             killed at 3.016s                        0 bytes           overrun=false
case 4  exits at once, grandchild holds the pipe
                                    exit_seen 23.8ms    returned 12.017s     0 bytes           overrun=true
```

Cases 1 to 3 are the repair working: the chatty-child deadlock the old code had is gone, a 4 MiB stdout no longer blocks, and a hung command is killed at its bound.
Case 4 is what is left: `run_bounded` returned at 12.0s on a 3s bound, four times over, having already seen the child exit at 23.8ms.
The exit status was still captured correctly, because `try_wait` caches it and the later `wait()` returns the same value rather than an empty blob, so that part of the design is sound.
The unbounded join is not.

This is not theoretical here. See N7.

### N3, minor: the kernel start time is parsed and then discarded

`Registered` holds pid, exe, store and socket. `read_identity` reads `lstart=,args=` and splits the five-field start stamp off, then `identity_matches` binds it as `_stamp` and never compares it.
A recycled pid is therefore excluded only by argv substrings: the private store path, the socket path and the executable name.
Those paths are unique per run, so a collision is very unlikely, but the start time is available and unused, which is weaker than the registration the review asked for.

### N4, minor: the answering-store assertion can pass vacuously

`status_store` returns an empty string when the `cowfs` binary is absent, and the check is
`answered.ends_with(&store) || store.ends_with(&answered)`.
In Rust `String::ends_with("")` is true, so an empty `answered` satisfies the second arm.
In the in-slot test this is unreachable, because the same test hard-fails earlier on `.expect("cli")`, so the import cannot have succeeded without the binary.
It is still a hole in the check that F6's repair added, and one arm of it compares a string to nothing.

## 3. What the repair got right, and it is more than I expected

- **The safety work is real, and it held when it mattered.** The single most convincing artefact in the repair is not a passing test: it is the private pre-fix receipt for daemon pid 2875, where the teardown deadline was already spent, the harness read the mount table as Unknown, and it consequently unmounted nothing, deleted nothing and signed nothing. That is the fail-closed behaviour the review asked for, exercised by accident on a real daemon.
- **A missing binary is now a hard failure in either mode**, with a message that names the fix, proven at exit 101 against the repaired binary.
- **A missing mount capability is no longer silent.** It is recorded as `skipped-capability`, announced as not measured, counted by a dedicated receipt gate, and `COWFS_ACCEPTANCE_REQUIRED=1` turns it into a failure. Proven both ways: seeded skip in default mode announces "This run is NOT an acceptance"; the same seed with `REQUIRED=1` exits 101.
- **The blocking chain is now stated correctly.** The ignore attribute reads "chain broken: can_ingest gate, then #97 worktree path, then #98 provenance", and the document puts the gate first with the reason that provenance persistence cannot make a refused call publish anything.
- **The repair attribution is corrected.** `b59bc3c` with its two files, and `e243cb1` named as a documentation retraction with no code change.
- **The artifact caveat is exactly right.** "That is useful and it is not a build record", plus the statement that the production-identity argument stops holding the moment production source moves, plus digests in every receipt.
- **The export readback is symmetric**, the cache hook asserts its outcome strings, and the ignored gate now really does reset both slots and then prove the base survived two builds.

## 4. My own evidence

Everything under `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/15/cowfs/bench/out/real-project-final-critic/`.
One 600-second foreground run under the wave lock. The 263-second cargo batch was not repeated.

### Negative controls first, no mount and no daemon

| control | what it is | result |
| --- | --- | --- |
| C1 | repaired test binary alone, daemon gate | exit **101**, "a missing binary is never a pass" |
| C2 | the real tri-state mount control, 5 cases | exit 0, `cases: 5`, no mount involved |
| C3a | receipt gate, seeded capability skip, default mode | exit 0, announced "This run is NOT an acceptance" |
| C3b | same seed with `REQUIRED=1` | exit **101** |
| C3c | receipt row with `warm_base_published: "true"` | exit 0, see N5 |
| C3d | receipt row with `warm_base_published: true` | exit **101**, gate fires |
| C3e | receipt row with `base_status_fresh: "true"` | exit 0, see N5 |
| C4 | four child shapes against the copied `run_bounded` | see N2 |

No recursive delete, no dead-mount walk, no kernel wedge, nothing mounted during any control.

### Small complete real Core sample, 21 assertions, 0 failed, 0 capability skips

Sample: `git archive 4a70c53` of `crates/cowfs-treehouse`, 18 files, 332,059 bytes.
Binaries: the same three artifacts as my prior review, byte-identical digests, and labelled **UNVERIFIED as to build provenance** because no source-build binding exists:

```
cowfs-daemon    b6b9970944b487249d3041f40ed78d6c8755116e3df19b7d7d953cf14b4eeb51
cowfs           59b88405ab1e3ad00ed242938684ae01f56fe2254bfa18e34a6fe7c445bb9f3c
cowfs-treehouse 65a42acb158c37e46b4a454057cd0d56bec7d90d8c7c5ef51dc4f1f0b15fab6e
```

These are the same digests the repaired receipts at `1b1f2e1` carry, and the same ones the pre-repair receipts carry, so the repaired run reused an artifact built at 16:12 on 2026-10-04 and nothing was rebuilt.
That is consistent for artifact identity and it is not evidence about new production source. The PR changes only a test and a document, so no production claim is involved either way.

Executed, all real:

```
adapter                         nfs, reported by the daemon
import                          exit 0, verified true, 18 files, 332,059 bytes
source_root_hash                8161766573cb3ba31c537be798971ddc5ae5c8eb9376346bf745eb1084bfaf55
imported_root_hash              8161766573cb3ba31c537be798971ddc5ae5c8eb9376346bf745eb1084bfaf55
promote / fork                  exit 0 / exit 0, parent rep-base
answering store                 the private store this run started, read back from the daemon
mount identity                  localhost:/cowfs-a675664d59f00669170589fbcd4af353, fstype nfs
symmetric readback              source 23, export 23, only-in-source 0, only-in-export 0
export digest                   108db1f9bdd0b18c02a5c7bd7740278b21ca8455c5430ba8a5f99a3c696d9174
native baseline digest          108db1f9bdd0b18c02a5c7bd7740278b21ca8455c5430ba8a5f99a3c696d9174
base digest                     108db1f9bdd0b18c02a5c7bd7740278b21ca8455c5430ba8a5f99a3c696d9174
write into the export           46 bytes, read back identical, base Cargo.toml unchanged, marker absent from base
reset                           exit 0
live readback at 0.01s          24 entries, digest differs, marker already gone   stale
live readback at 2.01s          23 entries, equals base and equals native source  converged
fresh daemon, same store        23 entries, equals base, equals native source, marker gone
teardown, both daemons          gone true, signalled false, mounts_left_listed []
shared daemon 15263             argv byte-identical before and after
```

Export, base and the native source all carry the same tree digest, which is the source-bound root readback, and the roots the daemon computed agree with each other.
The lag at 0.01s is recorded as an independent observation with its two timestamps and is not waited on and then asserted.
The fresh daemon confirms the reset is durable in the store.
Nothing was left mounted, no process was signalled, and 15263 was never touched.

## 5. Findings that are not blockers

### N5, minor: the "never claim a published warm base" gate cannot fire on this harness's own receipts

The gate is

```rust
.any(|r| r["warm_base_published"] == true || r["base_status_fresh"] == true)
```

which compares against a JSON boolean.
`record()` builds every field with `serde_json::to_string(v)` on a `&str`, so every value it writes is a JSON **string**.
Proven: a seeded row with boolean `true` fails the gate at exit 101; the identical row with `"true"` passes at exit 0.
So the check is live but unreachable for any row this harness writes.
The same applies to `capability_skips`, which compares `r["outcome"] == SKIPPED_CAPABILITY`, a string, and therefore works, because `outcome` is genuinely a string everywhere except the teardown rows.

### N6, minor: the receipt schema is not uniform

29 records, 24 carry `outcome: "measured"`, and the 5 that do not are exactly the teardown records.
A reader who counts outcomes gets 24, not 29, and the teardown rows are the ones that carry the safety evidence.
Tagging them would make the receipt uniformly self-describing, which is the property the repair is reaching for.

### N7, disclosure: a real Core daemon was stranded, and neither public document says so

Found in the lane's private `acceptance-pre-fix.jsonl`, which is not referenced by either document:

```
daemon_pid                        2875
daemon_exited                     false
signalled_after_identity_check    false
unmounted                         ""
mounts_left_listed                unknown: deadline already spent before spawning mount
quarantined                       mount table unknown: deadline already spent before spawning mount
runtime_root                      /var/folders/…/T/.tmp53obIz
```

What that row shows: a real `cowfs-daemon` on a real mount did not exit within its window, the teardown's absolute deadline was already spent by the time it needed to read the mount table, and the harness then did the right thing at every step, refusing to unmount, refusing to signal and refusing to delete.
The remaining problems are disclosure and diagnosis, not behaviour.

Disclosure: neither `ready-real-project.md` nor `evidence/real-project16-repair.md` mentions this run, this daemon or this outcome.
The evidence file's list of four defects found and fixed does not include it, and its statement that all five daemons exited on their own is true of the cited final run and silent about the discarded one.
Whoever removed pid 2875, and when, is recorded nowhere.
I verified it is not running now: `ps -p 2875` returns nothing.
So the ownership question is answerable today, cheaply, and it should be answered in the document rather than left to the next reader.

Diagnosis: the record does not say why the 240-second deadline was spent. N2's unbounded join is a candidate mechanism, because it is the one path in `teardown` with no bound, but I am labelling that a hypothesis and not a finding: `shutdown` and `ps` both have caps, and I cannot show from this receipt which call consumed the time.
The receipt file itself is also a hand-assembled mixture: five of its six teardown rows carry the old schema and the same pids as the `1ca8242` run, and only the sixth is from the new code.
The rows are preserved rather than rewritten, which is right, but the filename says "pre-fix" for a file that is mostly not a pre-fix run.

### N8, informational: main has moved and this PR correctly makes no claim about it

`main` is now `3a6935b`, the merge of PR #111, which changed production NFS code relative to the branch base `46b0f26`.
The document's production-identity argument is scoped explicitly to "between `46b0f26` and `1b1f2e1`" and does not extend to main. Correct as written.
The consequence for whoever merges: a run on merged main is a different measurement, because the NFS layer underneath the export is different, and the receipts in this PR say nothing about it.
The repair's own wording invites exactly the right conclusion, that the argument stops holding the moment production source moves.

### N9, informational: the evidence file does not mention the head it documents

`evidence/real-project16-repair.md` lists commits `1ca8242` and `1b1f2e1`. The head is `4a70c53`.
Correct for what it measures, and a reader following the PR head lands one commit past the list.

## 6. Required before merge

1. Fix the Linux `mount` grammar in `mount_identity` and add a synthetic case for the `src on point type fstype (opts)` shape. This is what makes `ubuntu-latest` red. N1.
2. Bound the joined drain readers, or drop the join and read with a deadline, so `run_bounded` cannot return after its bound. N2.
3. Disclose the stranded daemon: the pid, the runtime root, the quarantine receipt, and who cleaned it up. N7.
4. Compare the kernel start time in `identity_matches`, or stop reading it. N3.
5. Fix the vacuous arm of the answering-store check. N4.
6. Tag teardown receipts with an outcome. N6.
7. Make the "claims a published warm base" check compare the string shape the writer produces, or make the writer emit booleans. N5.

Item 1 is required for CI to be green and is entirely inside this PR.
Item 2 is required because it is the one remaining unbounded path in code whose whole purpose is bounding, and there is a recorded real daemon behind it.
Items 3 to 7 are small and each closes a hole that a future reader could mistake for a guarantee.

## 7. Scope not crossed

- Source read-only. No commit, push, merge, rebase, checkout, reset or stash. Lease HEAD unchanged at `6df8b9f`.
- Writes confined to `bench/out/real-project-final-critic/**` plus this one assigned report.
- No production source edited: not the Core ingest boundary, not `import.rs`, not #98, #20, #96, #79, #42, not the companion.
- No #97 production fix requested or duplicated; the unfixed `import.rs` is blob `df2c831d`, identical at `46b0f26` and at `main` `3a6935b`.
- No Linux cross-build, no 805M soak, no repeated 600-second waits, no cargo batch re-run.
- One foreground lock acquisition for the mounted workload; discovery and controls ran outside it.
- CI read once per run. No dispatch, rerun, poll, runner config or workflow change.
- 15263 and every other lease, daemon, socket, store and mount untouched, re-verified after the run.
- No privileged operation of any kind.
- No warm-benefit, performance, dedup, last-writer-wins or crash-durability claim is made or accepted.
- Raw evidence stays private to the lanes. No artifact link in any document I wrote.

## 8. Verdict

**REQUEST CHANGES.**
#15 and #16 warm-base acceptance: **NOT met**, and this delta does not change that.
The repair is real work, it is honest about what it measured, and its own new gate caught a genuine bug in itself on ubuntu, which is the strongest single argument that the gate works.
What blocks merge is narrow and mechanical: one platform-shaped parser, one unbounded join, and one undisclosed stranded daemon that the harness itself handled correctly.
Fix those three and this is mergeable as the honest negative result it is.