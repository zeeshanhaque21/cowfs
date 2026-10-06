# Publication correction for the cache-write-clock gap record

Scope: publish the independent review of the gap correction and correct four claims in my own record
`docs/verification/evidence/meta42-cache-write-clock-gap-correction.md`, sha256
`508698d027c3667fb258e3b4db36f78713789d04478077eb5a044022d90521c3`.

**That record is not rewritten.** It stays byte for byte as written, and this document is the
correction that supersedes its four claims.
Everything below is either a citation checked against the source or a statement of what the evidence
does not show.

| what | value |
|---|---|
| branch | `fix/cache-write-clock-42` |
| head this publishes on | `04d2fb554563df467b3f14a712f994c5d363ca19` |
| review published here | `docs/reviews/pr139-meta42-cache-write-clock-gap-final.md`, sha256 `661ead7050c648d5d3a53c75167daf0844924d69debd173decabb6a1fdfbd51d` |
| review's verdict on the source | source **PASS**, `io.rs` +50/-3, no further code change authorised |
| review's verdict on the record | four receipt claims need tightening |
| record being corrected | `docs/verification/evidence/meta42-cache-write-clock-gap-correction.md`, `508698d0…`, unmodified |
| lease | `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/6/cowfs` |
| work done here | documentation publication only, no code, no build, no test run |

## No production or test change accompanies this

`crates/cowfs-core/src/io.rs` is byte-identical at `04d2fb5` and at this document's commit, and the diff
between them is documentation only.
Nothing was compiled, no archive was extracted, no probe was built, no target directory was created,
and no clippy, fmt or test ran in this task.
The lane stood at 8,050,828 KiB against the 8,388,608 KiB cap with 337,780 KiB of headroom, and one
proven compile in this lane needs 463,868 KiB, so a build would have breached the cap.
No cleanup, deletion, move, offload or cap waiver was performed and none is authorised here.

## Correction 1: the "vDSO" claim was unsupported

My record said the reading is "the same vDSO clock call it always was".

**What the evidence establishes:** the production object that contains `clock_read` needs exactly one
undefined symbol, `cowfs_vfs::types::Timestamp::now`, and nothing else.
It also references no TLS or thread-local symbol and no `__atomic` or `__sync` libcall.

**What it does not establish:** any clock route.
A symbol name says nothing about which clock source the platform implementation reaches, and on macOS
the route and its inlining differ from Linux, so naming vDSO was an unverified embellishment.

**The defensible statement:** the production build makes the same single call to `Timestamp::now` that it
made before this change, and no more.

## Correction 2: `#[inline]` is a request, not a guarantee

My record said the reading is "reached through one inlined function".

`#[inline]` is a request to the optimiser.
The evidence I reported is symbol-level, and symbol-level evidence says nothing about whether a call
instruction survives in the emitted code.
A debug or incremental build may well keep the call, and mine was a debug build.

**The defensible statement:** the helper adds one private call that the optimiser is free to inline and
that carries no other effect, because the guard argument is borrowed and never read.

No benchmark was invented to settle this.
The review explicitly declined to invent one too, and the assignment rules it out.
Both statements are about structure, and neither claims zero cost, no call instruction, or a wall-time
property.

## Correction 3: the `old` arm was not byte-identical to the `gap` arm

My record said the `old` mutant "is byte-identical to" the `gap` mutant", while also giving the two arms
different `io.rs` sha256 values.
Byte-identical files cannot have different digests, so at most one of those statements was true.
I resolved it by reading the archives rather than guessing, since both arms are still on disk.

| arm | `park_if_armed()` | reading | `node.st.wr()` | actual position | `io.rs` sha256 |
|---|---|---|---|---|---|
| `new` | line 89 | none before the lock | line 91 | under the lock, through `clock_read(&st)` | `41b5a38ecdc6a64f…` |
| `gap` | line 89 | line 90 | line 92 | **between the park and the lock** | `3a791b9a11f7d20c…` |
| `old` | line 90 | line 88 | line 92 | **above the park** | `1bfbb7330e3f26a2…` |

So `old` is the above-park revert and the word "byte-identical" was the error.
The two arms are different files at different positions.

What *is* shared across all three arms, measured by hashing the regions separately:

| region | `new` | `gap` | `old` | identical in all three |
|---|---|---|---|---|
| the `ClockGate` block, from its doc comment to the test module | `540d57a448571844` | `540d57a448571844` | `540d57a448571844` | **yes** |
| `mod clock_order_tests`, to end of file | `c74dd725b681cd21` | `c74dd725b681cd21` | `c74dd725b681cd21` | **yes** |
| the whole `io.rs` | `41b5a38ecdc6a64f` | `3a791b9a11f7d20c` | `1bfbb7330e3f26a2` | **no** |

So the gate, the thread-local slots and the whole test fixture, including every assertion, are
byte-identical in all three arms.
The **production** `io.rs` is not byte-identical, and must not be described as such.
No feature was disabled and no hook was switched off.

This does not change the substantive result.
Both wrong positions fail, and the counter refuses both, so the three-arm outcome stands; only the
description of which mutant was which was wrong.

## Correction 4: the untouched clock-site citations did not resolve

My record named `io.rs:168`, `io.rs:436` and `ns.rs:176` as the other read-then-lock clock sites.
Checked against the actual source at `04d2fb5`, those line numbers do not land on clock reads and the set
was incomplete.

Every `Timestamp::now()` site in `io.rs` at `04d2fb5`, located by walking the file and tracking the
enclosing function:

| line | enclosing function | what it is |
|---|---|---|
| 172 | `op_setattr` | untouched, read before `node.st.wr()` at 179 |
| 391 | `op_setxattr` | untouched, read after `sc.ns.lk()` at 390, before the node lock |
| 440 | `op_removexattr` | untouched, read after `ns.lk()` at 439, before the node lock at 442 |
| 560 | `clock_read`, the `#[cfg(not(test))]` twin | the helper itself |
| 573 | `clock_read`, the `#[cfg(test)]` twin | the helper itself |

So the correct citation is `op_setattr` at 172, `op_setxattr` at 391 and `op_removexattr` at 440.
`op_setxattr` was missing from my list entirely.
Neither 168 nor 436 resolves to a clock read at this head.

**None of these three is touched by this branch**, and none has been reproduced.
They remain hypotheses of the same shape, exactly as the earlier records said, and this correction changes
only the line numbers and the completeness of the list, not that status.
`ns.rs` was not re-measured here, so no line number for it is asserted.

## Two further notes the review raised, recorded rather than dropped

**The compile-error sentence was stronger than the paragraph under it.**
My record said a reading moved back above the lock "is therefore a compile error".
That holds for a reading routed through `clock_read`.
A bare `Timestamp::now()` at an earlier line still compiles, and that is exactly why the counter exists.
My own record stated this correctly further down, so the headline and the body disagreed in strength.

The correct statement, in two halves, kept separate:

1. **Compile refusal, typed.** A reading routed through `clock_read(&guard)` cannot be written outside
   the guard's scope, because the function requires a live guard and has one call site. That is a
   compile-time property.
2. **Runtime refusal, counter.** A bare `Timestamp::now()` bypasses the helper, compiles, leaves the
   counter at 0, and is refused by the test at runtime.

Neither half is an ownership probe.
The counter does not inspect the guard, does not ask the lock who holds it, and proves nothing about
ownership or lock liveness.
It records that a reading went through the guard-requiring path, and that narrower claim is the only
runtime claim made.

**The merge-tree figure was computed from an uncommitted input tree.**
My record quoted tree `44e7bbcec61e0ddcb905c8d72cfb4cedabace146`.
That run happened while the record itself was still an uncommitted working-tree file, so the input tree
differed from the committed head by that one document.
This is not an inconsistency in the result, and it is recorded so nobody reads it as one.

**The current figure, computed read-only against the explicit fetched main ref:**

```
git merge-tree --write-tree cf67e8a6b2f8d346485fdf1c71d24283da0b43a0 04d2fb554563df467b3f14a712f994c5d363ca19
5cf4e415e464f07d3a164c2205bf4786a8e8ae4b
conflict lines: 0
exit 0
```

`cf67e8a6b2f8d346485fdf1c71d24283da0b43a0` is named explicitly rather than reached through a moving
`origin/main`, and it was verified present in the local object store before the call.
No checkout, no merge, no branch switch, no source written.

**What this is:** a clean, conflict-free resulting tree against that explicit main.
**What this is not:** a green run.
Nothing was compiled from that tree, and no test in it was executed.

## The three-arm results are mine, not independently re-executed

The `new` pass and the `gap` and `old` failures, with their exit codes and their `io.rs` digests, are
measurements I took in this lane.
The review at `661ead70…` read those archives and those logs and did not rebuild them, so the three-arm
outcome is **author-local evidence**, not an independent re-execution.
What the review independently established is the source-level reading: the ordering inside `op_write`,
the shape of both halves, and the fact that the `old`-arm description contradicted itself.

## The one green CI job is not a runtime gate for this change

At `04d2fb5`, run 37393266584:

| job | conclusion | covers this change |
|---|---|---|
| `linux-fuse` | completed, success | **no** |
| `check (ubuntu-latest)` | in_progress | not yet determined |
| `check (macos-latest)` | in_progress | not yet determined |

The `linux-fuse` job runs `cowfs-vfs-path --test native` for three native controls and then
`cargo test -p cowfs-fuse --include-ignored`.
Its step list is checkout, toolchain, cache, FUSE install, the three native controls, the FUSE mounts and
conformance step, and the conformance enforcement step.
It never runs `cowfs-core`, so it does not execute `io::clock_order_tests`, `caches`, `locks` or
`operation_time`.

**Therefore a green `linux-fuse` job is not evidence that this change's tests pass, and none is claimed
from it.**
The jobs that do run `cargo test --workspace`, and would execute the new test, were still in progress at
this snapshot.
CI on the head this document publishes is read once and reported, not polled.

## The physical under-lock fix is unchanged

The production change landed at `cd1dca8b7b5bd331e8e553e7874c0852458a1c15` and is untouched since.
This branch added a test, then closed the test's coverage gap, and changed no production ordering.
`crates/cowfs-core/src/io.rs` at `04d2fb5` still reads the clock inside the node write lock, after the
poison check, in the same position relative to `Stale`, the dirty-byte accounting, the queue touch and the
in-lock `try_enter`.
A poisoned node still returns before any clock read.

## A history hazard the coordinator must handle, not me

The branch's earlier commit `e9dc106` is titled `fix(core): read a write's clock after it takes the node
lock (issue #42 cache-write clock)`, and its body contains the phrase `(issue #42 cache-write clock)`.

That is a closing keyword immediately followed by an issue number on one line, so GitHub can treat it as
a closing reference for #42 even though the intent was neutral.
It is published history on a shared branch, so it is **not** rewritten here: rewriting shared history to
edit a commit message is a far larger action than this task authorises.

**This is a real risk, not a hypothetical one.** In this repository an empty
`closing_issues_references` list has already been observed alongside an issue that was closed anyway.
So "the refs are empty" is not sufficient proof that #42 stayed open.

**What the coordinator should do:** verify #42's state from the API immediately after any merge, not
from the refs list, and reopen it if GitHub closed it.
As of this writing the API reports #42 `open` with `state_reason: reopened`, and the only `closed` and
`reopened` events in its timeline are timestamped 2026-10-05T23:02:38Z and 23:03:29Z, which precede this
branch's current commits.

## What this document does not claim

- **No runtime gate from CI.** One job is green and it does not cover `cowfs-core`.
- **No independently re-executed three-arm result.** Those are author-local measurements, re-read by the
  review but not rebuilt.
- **No compile of the merged tree.** Integration is a conflict-free tree and nothing more.
- **No zero-cost, no call-instruction and no wall-time claim** about the helper, and no benchmark was
  invented to settle it.
- **No vDSO or clock-route claim.**
- **No ownership or lock-liveness claim** for either half of the property.
- **No global monotonicity.** `Timestamp::now()` is the host wall clock and can step backwards.
- **No change to the status of `op_setattr`, `op_setxattr` or `op_removexattr`.** The citations were
  wrong and are corrected; none was reproduced and none is touched.
- **No performance or timing acceptance, no acceptance threshold, no `SIGKILL` or power-loss claim.**
- **`cargo test --workspace` was not run here**, and no test of any kind ran in this task.
- **`no-mistakes` is not initialized** in this repository, `.no-mistakes` and `.claude` are both absent,
  so that pipeline did not run and no claim is made about it.
- **No browser step.** `chromium` is not installed, so any browser work would be **UNVERIFIED** here, and
  this change has no browser surface.
- **`codebase-memory-mcp` graph tools were not used.** Every citation here was resolved by reading the
  file, which is what the corrections required.
- **MisakaNet was local-only and was not consulted.** No remote call was made.

## Superseded and immutable

| document | sha256 | status |
|---|---|---|
| this record, `meta42-cache-write-clock-gap-correction.md` | `508698d027c3667fb258e3b4db36f78713789d04478077eb5a044022d90521c3` | immutable, superseded on four claims by this document |
| `docs/reviews/pr139-meta42-cache-write-clock-gap-final.md` | `661ead7050c648d5d3a53c75167daf0844924d69debd173decabb6a1fdfbd51d` | published here byte for byte |
| `docs/reviews/pr139-meta42-cache-write-clock-permanent-final.md` | `75a2310b5b74018171fba9116faec5b73850b5e712fa90b1555889ebe5d97920` | immutable |
| `docs/verification/evidence/meta42-cache-write-clock-permanent-regression.md` | `b5e1f19cf757eb87634473b033e0307b295ccad6435b8eaba78ea1272b5683fb` | immutable |
| `docs/reviews/pr139-meta42-cache-write-clock-final.md` | `72d37cc4d209ca2c3bda04e969e7889d16b846f7dc622c18b0ffb5e8ca5c6fa7` | immutable |
| `docs/verification/evidence/meta42-cache-write-clock.md` | `a8bc5337cbf5bb9ac28c1d2801377cce552681432fcd2b5b18fad35bfad42451` | immutable |

Evidence from the earlier lanes is untouched:
`bench/out/meta42-cache-write-clock-gap-correction/` keeps its three archives, their three isolated target
and temp directories, and its logs, including the three-arm and reordered-old-arm runs.
