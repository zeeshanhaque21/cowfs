# Translate #43 residual race: RPC outcome and valid-channel correction

Receipt for the third revision of the #43 Translate residual race fixture on PR #143.

## What this supersedes and what it does not

This is a new receipt.
It does not rewrite or replace `translate43-existing-race-repair.md` or `translate43-original-race-regression.md`, which stay as they were written.
It records the corrections the final review (`docs/reviews/pr143-original-race-final-wbuddy-review.md`) required and the evidence produced after them.

## Corrections the review required

1. The previous raw fixture dropped the `create(doc)` result.
   If that create failed, `doc_present_at_mutation` stayed `false`, the shadow assertion was skipped, and the test could pass green while the defect was live.
   That is a concrete false-green path.

2. The `NFS3ERR` 10004 seen on the channel was mislabelled.
   10004 is `NFS3ERR_NOTSUPP`, not `NFS3ERR_IO`.
   It is the deliberate refusal of an implausible AppleDouble prefix in `crates/cowfs-nfs/src/sidecar.rs`, not a corrupt write.
   A malformed payload being refused is not a product bug, and no namespace fix touches it.

## Same input two ways: valid vs invalid payload

Proven before writing the permanent fixture, over the real in-process raw-NFS server, on the fixed head.

| payload | where | write | read | bytes |
| --- | --- | --- | --- | --- |
| valid AppleDouble, `Sidecar::from_xattrs(...).encode()`, 4096 bytes | translating `._doc` (main file exists) | `0` OK | `0` OK | round-trip equal |
| junk `[0u8; 4096]` | translating sidecar name (main file exists first) | `10004` NOTSUPP | - | refused, no corrupt state |

The valid payload is the form a macOS client writes, produced by the existing public encoder in `appledouble.rs` and already used by the public `translate.rs` fixtures.
The invalid payload is refused by the existing `is_plausible_prefix` guard with `NFS3ERR_NOTSUPP` and leaves the valid channel readable, so the refusal is deliberate and non-destructive.
A claim that 10004 is a product bug would require a valid payload to fail; it does not.

One ordering trap: the refusal only fires when the main name exists first, so `._name` is a translating view.
Created without the main name, `._name` is an ordinary real file and a write to it is a normal file write that returns OK, which is why the control creates the main name before the channel.

## The fixture outcome contract

`crates/cowfs-nfs/tests/namespace_race.rs` drives `mkdir(._doc)` and `create(doc)` as two real RPCs on two connections to the in-process server.
The guard read of the main name is held after it answers; the second connection creates the main name; the held mutation then runs on the stale answer.
No injection below `Vfs`.

Every expected-success RPC status is asserted before the outcome branch:
`create(doc) == OK`, a raw `lookup("doc") == OK`, and `mkdir(._doc) == OK`.
A failed create therefore fails the run on its own, independent of `doc_present_at_mutation`.
The false-green path from correction 1 is closed.

The two legal outcomes are derived from the recorded fact, never guessed from timing:

- `doc_present_at_mutation == true` (the guard read stale state):
  no real object may take the live view name, and the `._doc` channel for the main file must open and round-trip a valid AppleDouble payload.
- `doc_present_at_mutation == false` (the `mkdir` was the serial winner):
  a real `._doc` directory is correct - the macOS fallback for a sidecar written before its main file - and it is asserted as such, with `doc` a regular file.
  This branch is not treated as the defect.

The channel-success branch (main file first) cannot be reached by the race on a correctly serialised adapter, because the `mkdir` wins the lock first.
It is therefore driven explicitly by `a_main_file_first_channel_round_trips`, so a fixed run that only ever serialises `mkdir` first still covers the channel path.

## Executed evidence

Old main, unmodified `89353e17`, fixture copied in unchanged, `cargo test -p cowfs-nfs --test namespace_race`, run in a detached worktree of the pre-fix head:

```
NFS mkdir(._doc)=0 create(doc)=0 overlapped=false doc_present_at_mutation=true real_dir_took_the_name=true depth_while_held=1 peak_depth=1 names=["._doc", "doc"]
thread panicked at crates/cowfs-nfs/tests/namespace_race.rs:357: a real directory took the live view name: the guard read stale state
test result: FAILED. 4 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
```

The named assertion is `a real directory took the live view name: the guard read stale state`.
`create(doc)` returned OK, so the failure is the shadow itself, not a failed request.

Fixed head `8c3f816`, same fixture:

```
NFS mkdir(._doc)=0 create(doc)=0 overlapped=false doc_present_at_mutation=false real_dir_took_the_name=true depth_while_held=0 peak_depth=1 names=["._doc", "doc"]
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

The distinguishing signal is `doc_present_at_mutation`: `true` on old main, `false` on the fixed head.
The final name set `["._doc", "doc"]` is the same on both and is not the signal, exactly as the review said.

## Local gates on the fixed head

Run under the resource lane, `CARGO_TARGET_DIR` inside the lease.

| gate | result |
| --- | --- |
| `cargo fmt --all -- --check` | rc 0 |
| `cargo clippy -p cowfs-nfs --all-targets -- -D warnings` | rc 0 |
| `cargo test -p cowfs-nfs` | all targets pass, 0 failed |
| `cargo test -p cowfs-nfs --test namespace_race` | 5 passed, 0 failed |

## CI on the pushed commit

Normal CI, genuine test run, no dispatch, no rerun, no workflow edit.

- commit `7afe72264e1564b471e94aeaace8a370384514e0`
- run `37526812136`, workflow `ci`, conclusion success
- jobs: `linux-fuse` success, `check (macos-latest)` success, `check (ubuntu-latest)` success

The macOS job ran the fixture target:

```
     Running tests/namespace_race.rs
test a_refused_directory_leaves_the_name_free ... ok
test a_main_file_first_channel_round_trips ... ok
test implausible_sidecar_bytes_are_refused ... ok
test the_guard_and_its_mutation_are_one_step ... ok
test a_sidecar_name_never_becomes_a_real_object_under_raw_nfs ... ok
test result: ok. 5 passed; 0 failed
```

The Ubuntu job compiles the target and runs it with the platform-gated fixtures ignored (0 passed, 21 ignored), as before.

## What this does not claim

- No claim that the adapter lock is the only defence; the adapter-reachable window is what this fixture exercises.
- No claim about the separate `._doc` channel `NFS3ERR_NOTSUPP` on implausible bytes being a defect; it is deliberate and proven non-destructive.
- No production change in this revision; the fixture is test-only.

## Provenance

- lease: READY7, `treehouse --root /Users/zeeshanhaque/Projects/cowfs/.treehouse-ready-wave`, slot 7.
- branch: `fix/nfs-translate-namespace-race-43`.
- fixture: `crates/cowfs-nfs/tests/namespace_race.rs`, commit `7afe722`, pushed over HTTPS.
- PR: #143, draft, `Refs #43`.
- old main: `89353e17e5085000711dc428e834f9cc41840a1f`, read-only detached worktree under `bench/out/ready43-race/`, removed after use.
- immutable: `translate43-existing-race-repair.md`, `translate43-original-race-regression.md`, both reviews.
