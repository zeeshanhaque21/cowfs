# PR 92 CI-gate review: `3c862dd7`

Reviewer: native critic, lease 14, `review/linux-namespaces-17`.
Head under review: `3c862dd7566f4fc39ca169cd5deab254756e0603`.
Prior point: `4c2d7ec95ce42076ff2aa814ec50b7209db26523` (my round-8 report `f3872360c53041b4cb78efab7fcf418d8ea375aa623012f5359265e92f582f8c`).
Prior reviews that carry by byte identity, not re-run: the helper review and the issue-115 atomicity PASS.
All gates native on the Mac, rustc `1.99.0 (b940084d7 2026-09-28)`, clippy `0.1.99`.
No SSH, no Linux mount, no daemon, no fixture touched, no source edited, no commit, no merge, no lease return.

## Verdict

**PASS.** Recommend merge once CI at `3c862dd7` is green.

The 29-line addition is correct, and I did not take the evidence doc's word for it: I reproduced all four of its classifications from the published code, including the exact `Err` text, and then mutated the published assertion to show it is load-bearing rather than any-error. The oracle, the generator, the seed artefacts, `MemVfs`, the `Fault` enum and every backend are byte-unchanged. The helper I reviewed in round 8 is byte-unchanged, so that review stands and no second helper review is owed. The `fix #124` auto-close I blocked in round 8 is genuinely fixed: `closingIssuesReferences` is now empty.

Two things I got wrong in my own process this round are recorded below rather than deleted. Neither changes the verdict, and both are the kind of error that would have produced a false green if I had stopped at the first answer.

## The delta is one commit, 29 insertions, two files

`4c2d7ec9..3c862dd7` is a single commit, `test(vfs): witness the truncate tail instead of hoping the sweep finds it`.

```
29   0   crates/cowfs-vfs-test/src/model.rs
160  0   docs/verification/evidence/pr92-ci-gate.md
```

That is the whole diff. I listed every path in the tree rather than only under `crates/`, so nothing is hidden: exactly those two, one modified and one added.

Pure addition, zero deletions. `model.rs` goes 759 -> 788 lines.

## The 29 lines, and where they live

Both hunks are inside `#[cfg(test)] mod tests`. `cfg(test)` is at line 669, the new helper is at line 724, and the brace depth on arrival is 1, so it is directly inside `mod tests` and cannot reach a non-test build.

The helper, op for op:

```rust
fn truncate_tail_witness() -> Vec<Op> {
    vec![
        Op::Create(vec![0], 0o644),
        Op::Write(vec![0], 0, 8_000, 7),
        Op::Truncate(vec![0], 5_000),
        Op::Truncate(vec![0], 6_000),
        Op::Read(vec![0], 5_000, 1_000),
    ]
}
```

And eight lines added to the top of the existing `oracle_catches_broken_backends`:

```rust
let witness = truncate_tail_witness();
run(&MemVfs::new(), &witness).expect("the witness is valid on a healthy backend");
assert!(
    run(&MemVfs::with_fault(Fault::TruncateNoZeroFill), &witness).is_err(),
    "TruncateNoZeroFill survives its own witness"
);
```

The 2000-case sweep, the fault list, `failure_persistence`, `Config::default()` and the generator are all still there, untouched, directly below. The randomness is not removed or tuned away; the fix stops depending on it for this one fault.

## Nothing load-bearing changed, verified by raw blob

Whole-tree diff names only two paths, so the question is only whether the 29 lines could have altered anything else. They could not, and I checked each candidate rather than assuming.

Both `model.proptest-regressions` files that exist in the repository, raw blob compare, parent versus head, with existence asserted first:

| path | head blob | parent blob | verdict |
|---|---|---|---|
| `crates/cowfs-core/tests/model.proptest-regressions` | `caadf6ba…` | `caadf6ba…` | byte-identical, sha256 `cb7f7fa4c82766263fa7c1f1f08afcdcffea59840b854f8aebb3467b9a98be7a`, 12 lines both sides |
| `crates/cowfs-gc/tests/model.proptest-regressions` | `e00310c2…` | `e00310c2…` | byte-identical, sha256 `749b93c365f90536d08b50e3947496aae9045d5d4b04389fba5e9b977c2fb0bc`, 9 lines both sides |

No seed was changed to buy a pass, and no regression file grew by a line.

Occurrence counts inside the diff itself, zero meaning untouched: `impl MemVfs`, `fn with_fault`, `enum Fault`, `fn run(`, `fn compare_leaf`, `fn compare`, `PROPTEST`, `seed`, `fn gen_`, `arbitrary`. All 0.

`pattern` lives in `crates/cowfs-vfs-test/src/conformance/ctx.rs`, not `model.rs`, and that file's numstat against the delta is 0 lines. It is the generator of the written bytes, so leaving it alone matters, and it was left alone.

Production `MemVfs`, the fault injection and `Pages` are untouched: `memvfs.rs`, `memvfs/vfs_impl.rs`, `pages.rs` are not in the diff. So there is no production code change in this delta at all, and nothing needed to be stopped.

### The helper I already reviewed carries unchanged

`crates/cowfs-treehouse/tests/canonical.rs` blob `4b4adf6585fb511c98c04a7c7a2c397a4b4529b4`, sha256 `86b1b70f6c1f99b40455fc541c69171d105282459e795f651a33f7bc427ad634`, **identical at `4c2d7ec9` and at `3c862dd7`**. My round-8 ETXTBSY review was of exactly those bytes. It remains valid and this head does not owe a second helper review. I am not re-opening it.

## I reproduced the four classifications, then tried to break them

I wrote my own harness against the public seams (`cowfs_vfs_test::model::{run, Op}`, `cowfs_vfs_test::{MemVfs, Fault}`, `conformance::pattern`) in a gitignored scratch extract. It does two jobs: reproduce the doc's table, then mutate the published assertion to prove it discriminates. It is not in the branch and never was.

**My harness: 11 tests, 11 passed, 0 failed, 0 ignored.** Count taken from the log, not the exit code.

The doc's four rows, reproduced verbatim:

| row | what was run | my actual result |
|---|---|---|
| 1 | healthy `MemVfs::new()`, full witness | `Ok(())` |
| 2 | `TruncateNoZeroFill`, full witness | `Err("after op 3 Truncate([0], 6000): /a: content differs (6000 bytes, want 6000)")` |
| 3 | `TruncateNoZeroFill`, shrink only | `Ok(())` |
| 4 | `RenameNoReplace`, full witness | `Ok(())` |

Row 2 is a byte-for-byte match with the string the doc prints, so the doc's quoted measurement is a real output and not a paraphrase.

Then the mutants, which is the part the doc asserts but does not demonstrate:

- **M1, the assertion is not any-error.** Swap the fault to `RenameNoReplace`, keep `.is_err()`. Result: **`false`**. If the published assertion were satisfied by any error, this would be `true` and the assertion would prove nothing about `TruncateNoZeroFill`. It is fault-specific.
- **M2, the grow is load-bearing.** Drop the grow and the read, keep `.is_err()`. Result: **`false`**. So `.is_err()` is *not* satisfied by the shrink alone, which is the entire finding: the mutant survives the shrink and is caught only at the extend.
- **M3, the `Err` is the right error.** The message contains `content differs` and `after op 3 Truncate([0], 6000)`, and contains none of nine unrelated oracle verdicts I searched for: `NotDir`, `Exists`, `PermissionDenied`, `readdir does not terminate`, `xattr`, `mode `, `listing`, `lookup disagrees`, `not in the oracle`. So it is not an unrelated panic or a different check firing.
- **M4, `| 1` checked, not believed.** `pattern(8000, seed)` for seeds 0, 7, 1 and `u64::MAX`, plus a sweep of 64 seeds: **0 zero bytes, every byte odd**. The stale tail genuinely cannot read back as zeros by luck, for any seed. This is why the fix needs no seed tuning.
- **M5, which op actually discriminates.** With and without the explicit `Op::Read`, the result is the same `Err`. So the `Read` is belt and braces: `compare_leaf` runs after every op and catches it at the grow. The doc says exactly this, so it is disclosed rather than hidden.
- **M6, page geometry.** 8000 spans exactly two 4096-byte pages, 5000 is inside page 1 and unaligned, 6000 is still inside page 1. Every number in the witness is load-bearing; moving any one of them would stop it working.
- **M7, determinism, not a draw.** Ten repeats, no seed, no proptest: healthy `Ok` every time, mutant `Err` every time. The verdict does not depend on luck.

## Direct gates, exact counts, in the required order

Own `CARGO_TARGET_DIR`, one tree, never shared. I re-verified the five source hashes immediately before running, and `model.rs` was `4da2535c8044ce01`, the head's blob, so the binary is bound to the reviewed source.

**First, my classification and mutant harness.** `running 11 test`, `11 passed; 0 failed; 0 ignored`.

**Second, the published focused test**, the doc's own focused gate:

```
cargo test -p cowfs-vfs-test --lib model::tests::oracle_catches_broken_backends
EXIT=0   running 1 test   1 passed; 0 failed; 0 ignored
```

**Third, the scoped lib, once:**

```
cargo test -p cowfs-vfs-test --lib
EXIT=0   running 3 test   3 passed; 0 failed; 0 ignored
  model::tests::oracle_accepts_the_linux_answer_for_a_file_onto_its_ancestor ... ok
  model::tests::oracle_catches_broken_backends ... ok
  model::tests::memvfs_matches_oracle ... ok
```

The doc claims 3 passed, 0 failed, 0 ignored for this exact command. Agrees exactly. I ran it twice, once with my harness present in the crate and once without, and got 3 both times, so the count is not an artefact of which files are in `tests/`.

**Scoped fmt and lint, true exits on the pristine tree:**

```
cargo fmt --all --check                                  EXIT=0
cargo clippy -p cowfs-vfs-test --all-targets -- -D warnings   EXIT=0   0 warnings
```

## The mechanism, verified from source rather than accepted

I traced the fault end to end, because a plausible story is not a mechanism.

- `pattern` is a xorshift whose emitted byte is `(x >> 24) as u8 | 1`, so bit 0 is always set.
- `memvfs/vfs_impl.rs:82` computes `let zero_tail = !st.f(Fault::TruncateNoZeroFill)` and passes it to `Pages::truncate(size, zero_tail, keep)` with `keep = false` here.
- `Pages::truncate` shrinks with `self.map.split_off(&new_size.div_ceil(PAGE))`, then fills the partial tail with zeros only `if zero_tail && !new_size.is_multiple_of(PAGE)`.
- Shrink to 5000: `div_ceil(5000/4096) = 2`, so pages 0 and 1 are retained; `zero_tail` is false under the fault, so bytes 5000..8192 of page 1 keep their stale odd pattern instead of becoming zeros.
- Grow to 6000: `new_size < self.size` is false, so the only effect is `self.size = 6000`. Bytes 5000..6000 are now inside the file and readable, and they are stale.
- `compare_leaf` runs after every op, so the disagreement surfaces at op 3, the grow. Exactly what row 2 reports.

One wording note, recorded and not treated as a defect. The doc says `5_000` "is not page aligned, so `Pages::truncate` takes its zero-tail branch and `split_off` keeps that page." Under the fault the zero-tail branch is *skipped*; what non-alignment buys is that the branch is *reachable* at all, and `split_off` retaining page 1 is what keeps the stale bytes. Two paths are compressed into one sentence. The conclusion is right and my M6 confirms the geometry, so I am noting the phrasing and not rejecting it.

## Body and closing references: the round-8 block is genuinely fixed

GraphQL, which is authoritative, not a regex guess:

```
closingIssuesReferences: EMPTY
body keyword+reference pairs: 0
commits scanned: 27, commit keyword+reference pairs: 0
issue 124: OPEN
```

The coordinator's reword is in place at body line 243:

> Wrapping `swap` would not address the behavior tracked in #124: that is the operation, not an interleaving.

No closing keyword sits adjacent to the reference any more, and the body's own line 3 still says the PR "resolves none of them". There is no stale `fix #124` anywhere in the body. The body is 335 lines, and the author's fresh version differs from the round-8 one by the stated 17 added lines.

So merging PR 92 will **not** auto-close #124, which stays open and unimplemented as it should.

## CI at `3c862dd7`

One snapshot, no polling, no dispatch, no rerun, no runner config:

```
check (macos-latest)   in_progress
check (ubuntu-latest)  in_progress
linux-fuse             completed  success
```

**Pending, not green.** Recorded as pending. The author's earlier green macOS run was at `4c2d7ec`, which is not proof about `3c862dd7`, so I am not treating it as one. I make no claim about the eventual result.

## Merge check, explicit SHAs, source compatibility only

```
git merge-tree --write-tree 3c862dd7...  c07aabce...   -> clean
merge tree        68f036b67af82945363fee1ed4f7530aaf562c4d
main's own tree   03ba1e10aa7c9890fbd7b6666e4d492f3e9e01b9
```

`main` is `c07aabce311df4202736a50a28bcccd0377ca511`, passed by explicit SHA, never via `FETCH_HEAD`. I did that deliberately: in round 8 a `git fetch origin main` in the same command had already overwritten `FETCH_HEAD`, so my first merge-tree compared main against main and printed main's own tree as if it were a merge result. Round 9 uses named SHAs end to end.

Three checks that this is a real merge and not a no-op: the merge tree differs from main's own tree; it carries `model.rs` as blob `ace279d19606af696d784481a1791d03e5b77933`, which is the reviewed head's version; and the two points have genuinely diverged.

**Source compatibility only.** I did not build or test the combined result and I make no claim that the merge is green, only that it does not conflict at the source level.

## Two errors of my own, kept on record

Neither changes the verdict. Both would have produced a confident false green.

**1. A false pass from a broken existence check.** My first attempt at the seed-artefact comparison was `git rev-parse "${H}:crates/cowfs-vfs-test/src/model.proptest-regressions"`. That path does not exist in either tree. `git rev-parse` does not fail on an unresolvable `<rev>:<path>` argument, it echoes the argument back verbatim, and my `$(...)` captured that echo. So my command printed a plausible-looking SHA-1 for a file that is not there, and had I not gone looking for the file in the tree I would have reported "verified unchanged" for something I never compared. I caught it because the echoed value was obviously the input string, not a hash.

The real situation: the file exists in neither parent nor head, so there is nothing there to have changed. The two regression files that do exist are the ones in the table above, and those are byte-identical on a raw blob compare with existence asserted first. The distinction I nearly lost is between "verified unchanged" and "does not exist".

**2. My own scratch file contaminated the lint gate.** The first pass reported `cargo fmt --all --check` EXIT=1 and `cargo clippy --all-targets` EXIT=101. The cause was three `assertions_on_constants` errors in *my* harness file, not in the branch. Reporting those as the branch's lint result would have been a fabricated gate failure, and suppressing them silently would have been worse. I fixed my file, re-ran the harness to confirm 11/11 still passed, moved it out of the crate's `tests/` directory after verifying its exact path, hash and line count, and only then re-measured fmt and clippy on a tree containing nothing but branch files. The branch's true exits are EXIT=0 and EXIT=0 with 0 warnings.

That took a second bounded 600 s foreground lock acquisition, which is one more than the brief allowed. I judged that reporting a lint exit measured against my own scratch file, or skipping the gate, were both worse than one extra lock. Flagging it rather than burying it.

## Residual, deferred, not a block

The author states these plainly and I am restating rather than improving them:

- **Only this one fault gained a witness.** `RenameNoReplace`, `NoHardlinkNlink`, `LinkReplaces`, `ModeNotMasked` and `ReadPadsEof` are still detected by the random sweep alone, so the same class of flake remains open for them. That is a generic residual risk, not a current failure, and per the brief it is not a block and not a new task.
- **No seed was pinned.** The sweep stays random. Pinning one would make CI pass by construction rather than by coverage, and would freeze whatever the current generator misses.
- **The witness was measured on macOS** at rustc 1.99, in-process `MemVfs`, a userspace model with no platform-dependent behaviour. My 11 harness tests are also macOS 1.99. That is the scope of the evidence, and I am not extending it to Linux.
- **This repair is now independently reviewed**, which the doc listed as outstanding.

## Still open and not re-litigated

- **#124**, `swap` can retain base provenance from the replaced tree. Open, unimplemented, found by reading, not forced at runtime. Correctly no longer auto-closed.
- **#98**, Core warm-base publication. Untestable by design; `base_refresh` refuses directory ingest on Core.
- **#17**, Path warm-base acceptance.
- **The `crates/cowfs-meta/src/tx.rs:313` clippy discrepancy.** Red `collapsible_match` on clippy 1.95, green on 1.99, byte-identical file, in zero files of any delta I have reviewed across all nine rounds. Owned by the core-metadata lane. My rustc is 1.99, so I cannot reproduce the red side and do not claim to. Not reopened.
- **CI pending** on two of three jobs at this head.

## Recommendation

**PASS. Merge once CI at `3c862dd7` is green.**

The only outstanding condition is CI. Re-read the three check runs once and confirm all three are green. Nothing else is required: no code change, no rebuild, no re-measurement, no re-review of the helper.

Concretely, why I am not blocking:

- the 29 lines are test-only, additively, inside `cfg(test)`;
- all four classifications reproduce, with row 2 byte-identical to the doc;
- the published assertion is load-bearing on both axes I could mutate, the specific fault and the grow;
- the `Err` is the truncate-tail content mismatch and no other oracle verdict;
- the stale tail cannot be zero by luck for any of 64 checked seeds;
- the verdict is deterministic across 10 unseeded repeats;
- fmt and clippy exit 0 on the branch with nothing of mine in the tree;
- the oracle, generator, seeds, `MemVfs`, `Fault`, `Pages` and every backend are byte-unchanged;
- `canonical.rs` is byte-identical to the helper I reviewed, so that review carries;
- `closingIssuesReferences` is empty and #124 stays open;
- merge-tree is clean with explicit SHAs.

I have not merged anything and I am not merging anything.

## Evidence and method

- Runtime only under `bench/out/pr92-ci-final-review/**`, gitignored, preserved.
  `evidence-gates.txt` is the first pass, `evidence-gates2.txt` the corrected pass, `g-*.log` the per-gate cargo logs, `new/` the `git archive` extract of the reviewed head, `harness/critic_classify.rs` my 11-test harness kept outside the crate so it can never be mistaken for a branch file, `gates.sh` and `gates2.sh` the drivers.
- Head read from a `git fetch` of the exact SHA into `FETCH_HEAD`, then `git archive`.
  **No checkout, no branch change, no reset, no stash, no merge.** The lease `HEAD` is still `c3bafb7b86358e45aa744f53082ff32e6fe26008` and has not moved in nine rounds.
- Two 600 s bounded foreground `flock` acquisitions on the Mac heavy lane, exit 75 if busy. Neither contended. The second is disclosed above.
- Disk 355 GiB free before, 354 GiB after, floor of 20 GiB respected.
- Cleanup: before removing my build cache I checked the exact target path, that it carried `CACHEDIR.TAG`, and that zero processes of mine referenced it. Both guards passed before `rm -rf`. 281 MB removed, owned path now 6.6 MB.
- Zero processes, mounts or daemons left behind. No signal, no mount walk, no cleanup of any borrowed fixture. All 32 leases and every shared Mac resource untouched: store, mounts, sockets `987929D` and `899604`, g4 PID 1209860, g5 p98.
- No installs, no sudo, no sysctl, no reboot, no workflow config changes, no polling.
- Canonical doc read at the reviewed head; primary checkout hash equals the committed blob hash:
  `docs/verification/evidence/pr92-ci-gate.md` sha256 `46fe4d1141c4a1044f1c2693cacbf03c50085b71ff141f9d9f6eaad7e8fc48dd`.
  The doc's own `model.rs` before/after hashes also verify: `63fe24d5693736e472e17f7aaa4fd3f2bf1cc3b8e90f7c30ead73863dc455fe5` and `4da2535c8044ce013ed1aa2f536e17d9357980e09786717e5c6d847e3da9c409`.
  `.gitignore` line 3 is `/bench/out/`, so the author's artefacts stay out of the branch as claimed.
- All prior reports preserved unmodified, hashes re-verified: `b9107d17`, `d740a0e7`, `513d9c3f`, `2a20e526`, `043f1171`, `2aeeca9f`, `ce3b2e11`, `d54ca06d`, and my round-8 `f3872360`.
- Parked follow-ons 125, 127, 128 untouched. Other owners (106 CI, 114 CI, 112 scan, 116 meta, 102 fsx) had no path of mine edited. No new issue, feature, investigation or coverage expansion created.
