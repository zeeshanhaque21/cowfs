# Tracker 122: GC dry-run mark-state assertion repair

Repair of the tautological assertion in `crates/cowfs-gc/tests/control.rs`.
Test-only change, one file, no production behaviour altered.

PR: https://github.com/zeeshanhaque21/cowfs/pull/130
Branch: `test/gc-dry-run-state-122`
Base: `00065ce75dcd554e1fb4bb084d1c70b2e2a21a87`
Commit: `8a8cee02e3591f30f76b990a06794e84d4fcdad9`

## What the defect was

`a_dry_run_changes_nothing_and_still_reports` ended with:

```rust
assert!(!f.gc_dir().join("mark.bin").exists() || true);
```

`|| true` makes the assertion accept every state, so it rejected nothing and the
claim in its own comment went unproven.

It also encoded a false premise about the mark file lifecycle.
The file is absent only until an earlier cycle records a set.
The fixture reuses one collector across cycles, so "absent" is not a durable
property of a dry run, and an assertion hard-coding absence would be wrong the
moment a set legitimately exists.

## Mark-file lifecycle, measured not assumed

Measured end to end through the public `Gc::open` and
`collect(dry_run: true)` on a real fixture, via a temporary probe test that was
deleted before lint and before the commit.

| step | `mark.bin` |
| --- | --- |
| fresh fixture, before `Gc::open` | absent |
| after `Gc::open` | absent |
| after `collect(dry_run: true)` | absent |
| after a real (non-dry) cycle | 120 bytes |
| after a second `dry_run` over that non-empty set | 120 bytes, unchanged |

The second dry run also reported `marked_skipped_roots == 1`, so it seeded from
the recorded set instead of walking the root again.

Source of the guard: `Gc::finish` in `crates/cowfs-gc/src/lib.rs` returns early
when `self.opts.dry_run`, which is the only thing keeping `marks.save` out of a
dry run.
The production behaviour is correct; no production defect was found and none is
claimed.

## The fix

The existing test captures the recorded bytes before the dry run and compares
them after, so it is indifferent to whether a set legitimately exists.

`a_dry_run_does_not_consume_a_recorded_set` covers the case the first test
cannot reach: it runs a real cycle first so a set exists on disk, then shows the
dry run reads that set and leaves those exact bytes behind.

Ownership stayed inside `crates/cowfs-gc/tests/control.rs`.
`tests/common.rs` needed no change.

## Proof

All `cargo` invocations ran as single foreground commands through the shared
`mac-heavy.lock` with the 600 second acquisition bound.

### Healthy tree, new assertion in place

```
cargo test -p cowfs-gc --test control
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

11 tests, 11 passed, 0 failed, 0 ignored.
No ignored count is treated as acceptance.

### Mutation control

Isolated source copy with its own `CARGO_TARGET_DIR`, so no shared target and no
stale artifact could be mistaken for a rebuild.
The mutant removes the `dry_run` early return in `Gc::finish`.

New assertions, same mutant:

```
test a_dry_run_changes_nothing_and_still_reports ... FAILED
assertion `left == right` failed: a dry run leaves the recorded set exactly as it found it
test result: FAILED. 10 passed; 1 failed
```

Same mutant, pre-fix tautological file restored from base `00065ce7`:

```
test result: ok. 10 passed; 0 failed
```

The old assertion passes the mutant and the new one rejects it, so the new
assertion is load-bearing.
The mutant demonstrates coverage only; it is not a proposed production change.

### Lint

```
cargo fmt -p cowfs-gc -- --check        exit 0
cargo clippy -p cowfs-gc --tests -- -D warnings   exit 0
```

### Source identity

```
sha256(crates/cowfs-gc/tests/control.rs) = d22e00861e91154d74dd16191f815e7dca5fd7b9107e99258a3c2c535f6b62cd
git blob                            = a6ce2df718bcbfb351c78561b34f01fb3a22a368
```

Raw logs and the isolated mutant trees are under
`bench/out/gc-dry-run122/` inside the lease, which `.gitignore` excludes.

## What this does not establish

- No physical reclamation. No real Core mount, no power-cut, no crash batch.
- No queued Core cancellation coverage. The cancellation tests in the file were
  run, not extended.
- No GC acceptance criterion, budget or g6 claim.
- No inference from a passing mutation control to correct mark-file lifecycle
  outside the fixture shapes tested.

## Review state

Refs #122 only.
No closing keyword appears in the commit message, the branch name or the PR body,
so the tracker issue stays open pending independent review.
PR 130 is not merged and awaits a fresh independent reviewer.

CI on PR 130 showed no configured checks at the time of writing
(`0 passed, 0 failed - this PR has no CI checks configured`).
That is a single snapshot, not a green run, and no rerun or workflow dispatch was
performed.