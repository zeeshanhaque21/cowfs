# PR 92 CI gate: the `cowfs-vfs-test` model failure

The narrow, test-only repair of the Ubuntu job that failed on `4c2d7ec`.
This is a different gate from the ETXTBSY one in `etxtbsy17-repair.md`, and it is not a cowfs
production defect either.

- Failed run: `37266648495`, job `111624789964`, `check (ubuntu-latest)`, step `Run cargo test --workspace`
- Failed head: `4c2d7ec95ce42076ff2aa814ec50b7209db26523`
- Changed file: `crates/cowfs-vfs-test/src/model.rs`, and nothing else. 29 insertions, 0 deletions.
- Both hunks are inside `#[cfg(test)] mod tests`. The oracle, the generator, `random_ops`, `exec`,
  `compare_dir`, `compare_leaf`, `MemVfs`, the `Fault` enum and every backend are unchanged.
- No production change was needed, so nothing was stopped for.

## The failure, verbatim

```
running 3 tests
test model::tests::oracle_accepts_the_linux_answer_for_a_file_onto_its_ancestor ... ok
test model::tests::memvfs_matches_oracle ... ok
test model::tests::oracle_catches_broken_backends ... FAILED

thread 'model::tests::oracle_catches_broken_backends' (20705) panicked at crates/cowfs-vfs-test/src/model.rs:744:26:
TruncateNoZeroFill was not detected: Ok(())

test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 8.26s
error: test failed, to rerun pass `-p cowfs-vfs-test --lib`
Process completed with exit code 101
```

The preserved log is `bench/out/pr92-ci-gate/logs/run-37266648495-job-111624789964-failed.log`,
sha256 `9edd4b799432fce261a92cefa993e8e98042609d7082ea97999c7c2ccc2e2892`.
The line the tool printed was truncated at 20000 of 300828 characters and the full copy it pointed at
lives in an ephemeral tool directory that was already gone, so what is preserved is the truncated
capture, unedited. Everything quoted above is inside it.

## Why it happened

`Fault::TruncateNoZeroFill` makes `MemVfs` shrink a file without zeroing the tail of the last page.
The oracle says the vacated bytes are zeros, so a correct backend and the mutant must eventually
disagree. They only disagree once the vacated bytes become **readable**, and reads clamp to the file
size, so they are not readable while the file is short.

The discriminating shape is therefore shrink, then extend, then read:

- shrink to a size that is not page aligned, so `Pages::truncate` keeps the partial page;
- grow again, which publishes those bytes;
- read the region, which the oracle answers with zeros.

Measured directly, with a temporary in-file harness that printed all four answers and was then
reverted byte for byte:

| what was run against the witness | result |
|---|---|
| healthy `MemVfs::new()` | `Ok(())` |
| `TruncateNoZeroFill`, the full witness | `Err("after op 3 Truncate([0], 6000): /a: content differs (6000 bytes, want 6000)")` |
| `TruncateNoZeroFill`, the witness **truncated to the shrink only** | `Ok(())` |
| `RenameNoReplace`, the full witness | `Ok(())` |

The third row is the whole finding: **the mutant survives the shrink on its own** and is caught only
at the extend, by the ordinary `compare_leaf` content check that already runs after every op. So a
random sweep has to happen to emit that exact four-operation shape on one path, and 2000 cases are
not enough to guarantee it.

The test was not deterministically broken here: the unmodified published test passed 5 of 5 runs on
this host, in 0.48s to 5.33s, which is the signature of a coverage race rather than a broken oracle.
The fault list, `cases: 2000`, `failure_persistence: None` and `Config::default()` are all unchanged,
so the randomness that made it flaky is still there; the fix does not paper over it, it stops relying
on it for this fault.

## The change

One helper and one assertion, both inside the existing test:

```rust
fn truncate_tail_witness() -> Vec<Op> {
    vec![
        Op::Create(vec![0], 0o644),
        Op::Write(vec![0], 0, 8_000, 7),
        Op::Truncate(vec![0], 5_000),
        Op::Truncate(vec![0], 6_000),
        Op::Read(vec![0], 5_000, 1_000),
    )
}
```

```rust
let witness = truncate_tail_witness();
run(&MemVfs::new(), &witness).expect("the witness is valid on a healthy backend");
assert!(
    run(&MemVfs::with_fault(Fault::TruncateNoZeroFill), &witness).is_err(),
    "TruncateNoZeroFill survives its own witness"
);
```

Why these numbers:

- `8_000` bytes spans two 4096-byte pages, and `pattern` returns `(x >> 24) as u8 | 1`, so **every
  byte is odd and none is ever zero**. The stale tail cannot be zero by luck, for any seed. This is
  why the fix does not tune a seed.
- `5_000` is inside page 1 and is not page aligned, so `Pages::truncate` takes its zero-tail branch
  and `split_off` keeps that page.
- `6_000` is still inside page 1, so the stale tail is still in the retained page when the file grows.
  Growing further, into a fresh page, would not discriminate.

The healthy assertion is not decoration. Without it the mutant assertion could be satisfied by a
witness the oracle rejects for an unrelated reason, which would be a false pass. The
`RenameNoReplace` row above is the same check from the other side: the witness does not catch an
unrelated fault, so the assertion is load-bearing rather than vacuously true.

## Gates, on this branch

| gate | result |
|---|---|
| `cargo fmt --all --check` | exit 0 |
| `cargo clippy -p cowfs-vfs-test --all-targets -- -D warnings` | exit 0 |
| focused `cargo test -p cowfs-vfs-test --lib model::tests::oracle_catches_broken_backends` | 1 passed, 0 failed, 2 filtered out |
| full scoped `cargo test -p cowfs-vfs-test --lib` | **3 passed, 0 failed, 0 ignored** |

The first attempt at the assertion used a one-element loop over a witness table, which
`clippy::single_element_loop` rejects. It was rewritten as straight-line code; the table idea is not
worth a lint suppression for one entry.

## What is preserved

`crates/cowfs-core/tests/model.proptest-regressions`, the model seed artefact carrying the
`9aa30bfa88a2438194d3b5ae7af55c7e2a59ff8a233abfe1cd0ddbec9d213900` case, is untouched:
sha256 `cb7f7fa4c82766263fa7c1f1f08afcdcffea59840b854f8aebb3467b9a98be7a`, identical before and
after. No seed was changed to get a pass.

`crates/cowfs-vfs-test/src/model.rs` before the edit:
sha256 `63fe24d5693736e472e17f7aaa4fd3f2bf1cc3b8e90f7c30ead73863dc455fe5`.
After: `4da2535c8044ce013ed1aa2f536e17d9357980e09786717e5c6d847e3da9c409`.

The ETXTBSY evidence and its repair are untouched, and the ETXTBSY merge block stands until it gets
its own independent review. #124 is unimplemented, #98 is incomplete, #17 acceptance is open.

## Residual, stated

- **Only this fault gained a witness.** `RenameNoReplace`, `NoHardlinkNlink`, `LinkReplaces`,
  `ModeNotMasked` and `ReadPadsEof` are still detected by the random sweep alone, so the same class
  of flake remains open for them. That is not fixed here and is not hidden: the comment in the test
  says where the next witness goes.
- **No seed was pinned.** The sweep stays random, so a different fault could still need a different
  draw. Pinning a seed would make CI pass by construction rather than by coverage, and would freeze
  whatever the current generator misses.
- **The witness was measured on macOS**, `rustc 1.99.0`, in-process `MemVfs`. `MemVfs` is a
  userspace model with no platform-dependent behaviour, and the CI run's own other two tests in the
  same binary passed, so this is a coverage property rather than a host property.
- **This repair has had no independent review.** It needs a narrow review before merge.

## Artefacts

Under `bench/out/pr92-ci-gate/`, which `.gitignore:10` keeps out of the branch:

| artefact | sha256 |
|---|---|
| `logs/run-37266648495-job-111624789964-failed.log` | `9edd4b799432fce261a92cefa993e8e98042609d7082ea97999c7c2ccc2e2892` |
| `logs/original-published-test-mac.log`, 5 of 5 pass on the unmodified test | `a70b576e307b65169147caefe524797b419077ffc556bd24ea4c78481129e83f` |
| `logs/witness-proof-mac.log`, the diff and the focused run | `33ff18a38e08b1f9f5cf34736a0985efe1f974d88b085db0d3f8b7500d9f9972` |
| `logs/scoped-gates-mac.log`, fmt, clippy, focused and full scoped suite | `26b03bdb37abc5d7d6cbffcc15fad1a0e05f77265f5beb89a56fb49649962ce8` |