# snapname42 final: independent review of PR #99 at a98ece8

Reviewer: independent critic, second pass.
The first reviewer produced a planning-only document with no executed artifact directory, so every
number below was produced by a command in this review, not carried over from that document or from
`docs/verification/ready-42.md`.

Verdict: **PASS with one documentation defect** (a wrong byte count in a doc comment).
Nothing blocking.

- PR: <https://github.com/zeeshanhaque21/cowfs/pull/99>
- Head reviewed: `a98ece8e6a737fa91c6825dea5ac3fb173939d93`
- Main at review time: `8255706f295227be54c014699d2a1c4c3ff58cd1`
- Merge base with `origin/main`: `46b0f269d5bef4a2c204c25f5b3015da601d3beb`, which is exactly the base
  `ready-42.md` names.
- Toolchain: `rustc 1.99.0 (b940084d7 2026-09-28)`, `cargo 1.99.0`, macOS aarch64.
- Artifact dir: `bench/out/snapname42-critic/` in the idle bench worktree
  `.treehouse-build-train/.treehouse/cowfs-7c1bf8/3/cowfs`, extracted with `git archive` from the
  exact head. The bench worktree's own branch (`review/fsck-live-refs-84`) was never checked out,
  edited, or reset.

## Source identity

The head was fetched over HTTPS by SHA and extracted with `git archive`, so nothing in this review
depends on a branch ref.

```
fetch origin a98ece8e6a737fa91c6825dea5ac3fb173939d93   -> FETCH_HEAD, exit 0
head_tree_files=533 mismatched=0
```

Every one of the 533 tracked files at the head was compared by `git rev-parse <head>:<path>` against
`git hash-object` of the extracted file: 0 mismatches.
The base tree used for the old-fail run was extracted the same way and checked the same way:

```
old_base_files=529 differing_from_base=0
fixture crates/cowfs-daemon/tests/snapname_drift.rs matches_head=True (ce12bbf885e2e83753169d989fcc67db26cf6612)
```

The old run therefore differs from the base by exactly one file, the fixture, which is byte-identical
to the head's copy.

## Locked build, pristine lock

`Cargo.lock` sha256, recorded before and after every build in this review:

```
4ae50a2b91cecc01b08102b9fc8d08ca79202483946239a908e443fdd1e18c55   before
4ae50a2b91cecc01b08102b9fc8d08ca79202483946239a908e443fdd1e18c55   after
```

Unchanged across all nine build and test steps below.
`cargo metadata --locked --format-version 1 --offline` exited 0 with empty stderr, and resolved the
dependency edges the PR claims:

```
cowfs-core deps: cowfs-gc,cowfs-meta,cowfs-snapname,cowfs-store,cowfs-vfs,fs2,thiserror
cowfs-ctl  deps: blake3,cowfs-snapname,cowfs-vfs,libc,serde,serde_json,thiserror
cowfs-snapname deps: blake3,unicode-normalization
workspace_members_has_snapname: True
```

`unicode-normalization` is gone from `cowfs-core` and `cowfs-ctl`, as claimed.
`blake3` stays on `cowfs-ctl` and is genuinely still used, by `crates/cowfs-ctl/src/treehash.rs`
(`blake3::Hasher` in five places), so that edge is not a leftover.

## Old fail, new pass

Same fixture file, same command, two trees, separate target directories so neither build could serve
the other a stale artifact.
Every exit code was read directly from `cargo`, never from a pipeline.

| tree | command | result | exit |
|---|---|---|---|
| base `46b0f26` + fixture only | `cargo test --locked -p cowfs-daemon --test snapname_drift` | **0 passed, 4 failed** | 101 |
| head `a98ece8` | same command | **4 passed, 0 failed** | 0 |

The four failures on the base, verbatim:

```
a_real_store_holds_exactly_the_names_the_control_api_accepts
  assertion `left == right` failed: "conf_base.cowfs-swap0": the control API says ok, the backend says refused
  left: true  right: false
a_staging_name_is_refused_by_the_control_api_before_it_reaches_the_backend
  must be refused: ()
the_control_api_and_the_backend_produce_the_same_collision_key
  left: "#9df1ddaff6ebdacb9a5b982c6621f10b2366e68ca30db6a79d4cf4376a21990c"
  right: "i\u{307}i\u{307}...i\u{307}"
bytes_from_a_wire_go_through_the_same_rule
  assertion `left == right` failed: "conf_base.cowfs-swap0"  left: false  right: true
```

Both failure modes the change targets are therefore real and demonstrated, not asserted:
the reserved-marker divergence and the unbounded collision key.
On the head all four pass, and the fixture's store-reopen half is genuinely in the test: it drops the
`Core`, reopens from the same directory, and repeats the reserved-name and listing checks.

## Targeted naming tests at the head

| command | result | exit |
|---|---|---|
| `cargo fmt --all -- --check` | clean | 0 |
| `cargo clippy --locked -p cowfs-snapname -p cowfs-ctl -p cowfs-core -p cowfs-daemon --all-targets -- -D warnings` | clean | 0 |
| `cargo test --locked -p cowfs-snapname` | 6 passed, 0 failed | 0 |
| `cargo test --locked -p cowfs-ctl --lib` | 10 passed, 0 failed | 0 |
| `cargo test --locked -p cowfs-core --lib snapname` | 4 passed, 0 failed, 25 filtered | 0 |
| `cargo test --locked -p cowfs-core --test names_ino` | 4 passed, 0 failed | 0 |
| `cargo test --locked -p cowfs-core --test critic2b staging_names_are_reserved_for_the_swap_protocol` | 1 passed, 0 failed, 27 filtered | 0 |

Wider sweep on the two forwarders' consumers, all exit 0:

| command | result |
|---|---|
| `cargo test --locked -p cowfs-daemon --lib` | 48 passed, 0 failed |
| `cargo test --locked -p cowfs-core --test swap` | 3 passed, 0 failed |
| `cargo test --locked -p cowfs-core --test chunks` | 4 passed, 0 failed |
| `cargo test --locked -p cowfs-gc --test regressions a_hole_is_never_swept` | 1 passed, 0 failed, 19 filtered |

Both controls are genuinely pre-existing and unmodified by the PR, checked by blob hash:
`crates/cowfs-core/tests/names_ino.rs` (`6359c0c8d8d7066b214a559f3c9e23d8fcf5f3fb` at base and head,
` snapshot_names_follow_the_cli_rules` present at base line 120) and
`crates/cowfs-core/tests/critic2b.rs` (identical at base and head,
`staging_names_are_reserved_for_the_swap_protocol` present at base line 911).
Neither is the fix; both are reported as controls.

## The specific claims, measured

These were re-measured by running code against the real `cowfs-snapname` at the head, not by reading
it. The probe files were deleted afterwards and the tree re-verified at 533 files, 0 mismatches.

| claim | measured | agrees |
|---|---|---|
| 126 `U+0130` fold to 378 bytes | `input_bytes=252 chars=126 folded_bytes=378 folded_chars=252` | yes |
| the key of that name is bounded | `key_bytes=65 is_hash=true` (blake3 hex 64 plus `#`) | yes |
| every `why` string is the pre-change wording | all six compared against the base wording | yes |
| `NAME_MAX` is bytes, not chars | 128 `é` = 256 bytes refused, 255 `x` accepted | yes |
| rule order: reserved before slash | `a.cowfs-swap0/repo` -> "is reserved for an interrupted snapshot swap" | yes |
| dot, slash, NUL, control all refused | `\0 \n \u{1b} \u{85} \u{7f}` all refused | yes |
| `conf_base.cowfs-swap0` is a real staging name | `<200 chars of conf_base>` + `.cowfs-swap` + `0` | yes |

The `why` strings measured verbatim:

```
""                        -> "empty"
"a/b"                     -> "must not contain a slash"
".x"                      -> "must not start with a dot"
"a\nb"                    -> "must not contain control characters"
"a.cowfs-swap0"           -> "is reserved for an interrupted snapshot swap"
256 x's                   -> "longer than 255 bytes"
[0xff, 0xfe]              -> "not valid UTF-8"
```

### The reserved marker is a substring that carries its own leading dot

`RESERVED` is `".cowfs-swap"`, dot included, so the check is `name.contains(".cowfs-swap")`.
Measured consequences, all correct and all covered by the tests:

- `cowfs-swap` (no dot) is **legal**, because the marker needs its leading dot.
- `cowfs-swap0` (no dot) is **legal** for the same reason.
- `conf_base.cowfs-swap0` is reserved, and is exactly what `staging_name("conf_base")` produces,
  because `staging_name` is `<first 200 chars>.cowfs-swap0` and `conf_base` is non-empty.
- Every real staging name contains the marker, because the target part of a legal name is never empty.

So the reserved rule fires on every staging name the swap protocol can actually generate, and on
nothing else. That is the right shape for the fix.

## API compatibility

Compiled probe, five tests, all passed, asserting each public item still exists with its old type by
coercing to a function pointer of the exact pre-change signature.

| item | result |
|---|---|
| `core::validate_snapshot_name: fn(&str) -> Result<(), ControlError>` | coerces |
| `core::validate_snapshot_name_bytes: fn(&[u8]) -> Result<(), ControlError>` | coerces |
| `core::name_key: fn(&str) -> String` | coerces |
| `ctl::validate_snapshot_name: fn(&str) -> CtlResult<()>` | coerces |
| `ctl::name_key: fn(&str) -> String` | coerces |
| `ctl::escape_control`, `validate_abs_path`, `validate_git_ref`, `validate_repo` | coerce |
| `ctl::MAX_NAME_BYTES == 255`, `core::NAME_MAX == 255` | unchanged |
| `ControlError::InvalidName(why)` still matchable, payload `"must not contain a slash"` | unchanged |
| `validate_snapshot_name_bytes(&[0xff,0xfe])` error equals `InvalidName("not valid UTF-8")` | unchanged |
| ctl message format `invalid snapshot name {name:?}: {why}` | unchanged, verified on the exact string |

The new refusal carries the same format with the new reason:
`invalid snapshot name "conf_base.cowfs-swap0": is reserved for an interrupted snapshot swap`,
with `code == ErrorCode::InvalidParams`.

### One residual observation, not a blocker

`cowfs-snapname::NAME_MAX` is a **fourth** independent definition of the 255-byte bound.
The other three are `cowfs-vfs::NAME_MAX`, `cowfs-meta::NAME_MAX`, and, through
`pub use cowfs_vfs::NAME_MAX`, `cowfs-core::NAME_MAX`.
All four are 255 today, and `cowfs-vfs` pins its own with an assertion, but nothing ties them
together: changing one would not change the others, and `cowfs_core::snapname.rs` bounds its key
against `crate::NAME_MAX` (from `cowfs-vfs`) while the rule it calls bounds against
`cowfs_snapname::NAME_MAX`.
This is pre-existing duplication that the PR widens by one, not a regression, and the PR's own goal
was the snapshot-name rule rather than the whole `NAME_MAX` family.
Worth one issue so the next lane does not treat `cowfs-snapname::NAME_MAX` as the single owner of 255.

## Mutant n16

The claim was that the old patch target no longer existed and the mutant would have reported
`PATCH-FAILED count=0` and stopped killing. Both halves checked.

Anchor:

```
anchor "    if is_reserved(name) {" occurrences in crates/cowfs-snapname/src/lib.rs: 1
OLD anchor "    if name.contains(crate::swap::STAGING) {" occurrences in crates/cowfs-core/src/snapname.rs: 0
```

`src_of` routes a path containing `/` to the repository root and everything else to
`crates/cowfs-core/src`, so `crates/cowfs-snapname/src/lib.rs` resolves correctly and the other 20-odd
mutants still resolve into `cowfs-core/src`.
The focus list is unchanged: `--test critic2b staging_names_are_reserved_for_the_swap_protocol`.

**The mutant was then run end to end, so this is measured, not inferred:**

```
COWFS_MUT_TIMEOUT=1500 python3 scripts/mutants.py n16_name_rule_allows_staging
  -> n16_name_rule_allows_staging | KILLED by --test critic2b staging_names_are_reserved_for_the_swap_protocol rc=101 | 14s
  -> exit 0
```

The kill is the expected one, from the expected test:

```
thread 'staging_names_are_reserved_for_the_swap_protocol' panicked at crates/cowfs-core/tests/critic2b.rs:912:5:
assertion failed: cowfs_core::validate_snapshot_name("evil.cowfs-swap0").is_err()
```

The patched file was restored by the script:

```
crates/cowfs-snapname/src/lib.rs sha256 190fcb7306263e7835c6d5e8d73d774bf0993ed0c980e9008af90c98fbe06f0e  before
crates/cowfs-snapname/src/lib.rs sha256 190fcb7306263e7835c6d5e8d73d774bf0993ed0c980e9008af90c98fbe06f0e  after
```

No other mutant was run; this review measured `n16` only, and says nothing about the rest of the set.

## Requests 1 to 4 are still open, and still open in the code

Checked by source, not accepted from the record:

| request | check | result |
|---|---|---|
| 1 `Meta::rename_snapshot` | `grep 'fn rename_snapshot' crates/cowfs-meta/src` | no match; not landed |
| 3 hole flag in `ChunkRef` | struct read | still `{ id: BlockId, len: u32 }`, 6 lines, no flags |
| 4 `reserve_inodes` / `create_with_ino` | `grep` in `crates/cowfs-meta/src` | no match; not landed |
| 5 `batch_at` / `set_now` | `grep 'fn batch_at\|fn set_now' crates/` | no match; not landed |

Behavioural confirmation for request 1, run against a real store:
`Core::rename_snapshot("conf_base", "conf_base.cowfs-swap0")` is refused and `snapshot_view("conf_base")`
still succeeds afterwards, so a refused rename leaves the source snapshot in place.
`cowfs-core/src/ino.rs` still owns its own two-copy durable high-water mark (`virt.ino.a`, `virt.ino.b`).

## Untouched files

Ten files that other lanes own, compared by blob hash between base and head. All identical:

```
UNTOUCHED crates/cowfs-core/src/ns.rs        UNTOUCHED crates/cowfs-core/src/inner.rs
UNTOUCHED crates/cowfs-core/src/io.rs        UNTOUCHED crates/cowfs-core/src/view.rs
UNTOUCHED crates/cowfs-core/src/vfs_impl.rs  UNTOUCHED crates/cowfs-core/src/queue.rs
UNTOUCHED crates/cowfs-core/src/gate.rs      UNTOUCHED crates/cowfs-core/src/lib.rs
UNTOUCHED crates/cowfs-daemon/src/import.rs  UNTOUCHED crates/cowfs-core/src/ino.rs
```

The PR diff touches exactly 11 paths, all of them claimed:

```
Cargo.lock  crates/cowfs-core/Cargo.toml  crates/cowfs-core/src/snapname.rs
crates/cowfs-core/src/swap.rs  crates/cowfs-ctl/Cargo.toml  crates/cowfs-ctl/src/validate.rs
crates/cowfs-daemon/tests/snapname_drift.rs  crates/cowfs-snapname/Cargo.toml
crates/cowfs-snapname/src/lib.rs  docs/verification/ready-42.md  scripts/mutants.py
```

`swap.rs` changes are confined to the two lines the record claims (`STAGING`, `is_staging`).

## CI on this exact head, read once

One read, no rerun, no dispatch, no poll.

```
run 37254580901  ci  followup/core-meta-integration-42
  head_sha=a98ece8e6a737fa91c6825dea5ac3fb173939d93  event=pull_request  attempt=1
  status=completed conclusion=success
  check (ubuntu-latest)  success
  check (macos-latest)  success
  linux-fuse             success
```

Three jobs, all green, on the first attempt, on the exact head reviewed.
The 3021-test total in `ready-42.md` belongs to run `37253399564` on `c6869a4`, an earlier head in the
same branch; this review did not re-derive that count and does not restate it as a fact about
`a98ece8`.
No local full-workspace run was attempted, per the dispatch rule.

## Closing references

```
PR 99  state=OPEN merged=false  closingIssuesReferences.totalCount=0  nodes=[]
issue 42  state=open  pull_request=false
```

Because an empty `closingIssuesReferences` has already been observed on this repo to be unreliable
(PR 104 is `MERGED` with `totalCount=0` while issue 21 was closed by an old commit and later reopened,
and issue 21 is `open` now), the commit messages were checked directly as well.
All 7 commits in `46b0f26..a98ece8` were scanned with
`(clos(e|es|ed)|fix(e|es|ed)|resolv(e|es|ed))\s*:?\s*#\d+`:

```
total_closing_phrases=0
```

No commit subject or body in this PR carries a closing keyword bound to an issue number, so nothing
here can auto-close #42 when the PR merges.
Issue 42's timeline shows three `referenced` events and no `closed` or `reopened` event; the two
non-ancestor commits among them (`06f0695`, `0092804`) belong to other work and also carry no closing
phrase.

## Defect found

**`crates/cowfs-snapname/src/lib.rs:95`, doc comment on `name_key`, wrong byte count.**

```
/// Folding can grow a name past [`NAME_MAX`] (255 bytes of dotted capital I fold to 382 bytes), so
```

Measured: 126 `U+0130` fold to **378** bytes, which is also what `docs/verification/ready-42.md:57`
correctly says.
The two numbers disagree inside the same PR.

Severity: documentation only.
The number is in a `///` comment, no code reads it, no test asserts it, and the shipped
`keys_fold_case_and_normalisation` test only asserts the key is bounded, which holds either way.
The bound that matters, `NAME_MAX == 255`, is correct and separately asserted.
The fix is one number in one comment, and it belongs to the PR author, not to this review.

Not raised as an issue by me: this review does not open issues or edit source.

## Not measured, and why

- Full-workspace `cargo test` / `cargo clippy --workspace`: the dispatch rule is against a full
  workspace build on the Mac, and CI covers both on the pushed head.
- The other ~20 mutants: only `n16` was in scope.
- The 3021-test CI total for this head: the run summary was read for status only, not parsed for
  per-suite counts.
- `docs/verification/ready-42.md`'s lease-path and fresh-clone provenance: this review used its own
  `git archive` extraction instead, and verified that tree independently.

## Environment

Nothing shared was touched.
The daemon PID 11068 recorded in `/Users/zeeshanhaque/.cowfs/daemon.pid` was **not running** when this
review started, and `/Users/zeeshanhaque/.cowfs/daemon.log` ends with `signal 15, shutting down`; the
store files were still present (`meta.redb` 958 MB, last written 2026-10-04 19:37, and both
`virt.ino` shards).
That state predates this review and was left exactly as found: no signal, no restart, no mount walk,
no cleanup, no `treehouse return`.

Every build ran under the shared mac-heavy lane lock
(`.treehouse-ready-wave/mac-heavy.lock`) with a single 600 s foreground wait and exit 75 if the lane
stays busy; the lock was acquired on every attempt, so nothing here was built outside the lane.
Owned artifacts: 2.9 GiB, inside the 8 GiB allowance.
Free disk at the end: 417 GiB.
No downloads, no `/tmp` use, no corpus copy, no install, no sudo, no sysctl, no reboot, no runner
intervention, no workflow dispatch, no rerun, no poll.

No source was edited, nothing was merged or pushed, and no lease was returned.
The bench worktree remains on `review/fsck-live-refs-84` at `0d51b38`, with its three pre-existing
untracked review documents, exactly as found.

## Canonical location

`/Users/zeeshanhaque/Projects/cowfs/docs/reviews/snapshot-name42-final.md`
(this file).
Raw logs and the extracted trees are under
`/Users/zeeshanhaque/Projects/cowfs/.treehouse-build-train/.treehouse/cowfs-7c1bf8/3/cowfs/bench/out/snapname42-critic/`,
which is gitignored by `.gitignore:10:/bench/out/`.
