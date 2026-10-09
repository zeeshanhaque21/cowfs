# Issue 117, the retry contract test that asserted nothing

Branch `fix/nfs-contract-assertion-117`, from exact main `951045fca4823611e196eda75db0c977a46d2c77`,
which is the merge of PR 96 and, behind it, PR 111. One file changed:
`crates/cowfs-nfs/tests/contract.rs`, blob `b28a679544d4913b9ce737b1bd6b75bb00873239` at `b4b55abfe9ab2d8d6f5fc42403bb1eb8b1c02d41`.

## The defect, probed before it was changed

```rust
let (_, _, _, post) = c.lookup(&root, "nope");
assert!(post.is_none() || true);
```

An instrumented run of the unmodified file reported:

```
PROBE st=2 fh=false obj=false dir_attrs=true truth_of_old_expr=true
```

Two things follow. The expression cannot reject any value, so the test was green for a reason
unrelated to the behaviour. And `post` was not what its name claimed: the fourth element of
`common::Nfs::lookup` is the post-op **directory** attributes, and on an error reply it is always
`Some`. The assertion was `false || true`, inspecting the opposite of what it was named for.

## The contract that is actually there, and is now asserted

Reachable through the same public TCP seam and the same fake `Vfs`. No new dependency, no new
framework, no production change.

| assertion | what it rejects |
|---|---|
| a `NFS3ERR_JUKEBOX` reply carries no object handle and no object attributes | a client reading a retry as a hit |
| with the injected fault cleared, an absent name answers `NFS3ERR_NOENT` and not `JUKEBOX` | a cached retry status that would send a client back for a name that will never exist |
| an error reply still carries the post-op directory attributes | the regression the old assertion was aimed at, now actually checked |

**No atomicity is claimed and none was invented.** `LOOKUP` mutates nothing, so there is no
partially applied state to specify. The barrier and error-precedence guarantees from #90 are
asserted in `crates/cowfs-core` and `crates/cowfs-nfs/tests/ns_durability.rs` and are untouched
by this branch. The `Retry` to `NFS3ERR_JUKEBOX` mapping is independently pinned by
`errors::tests::every_error_has_its_status`.

## Discrimination

A temporary mutation, reverted: `Error::NotFound` mapped to `NFS3ERR_JUKEBOX`, which makes a retry
status sticky.

| assertion | under the mutation |
|---|---|
| `assert!(post.is_none() || true)` | **passes**, so it never checked anything |
| `assert_eq!(st, NFS3ERR_NOENT)` | **fails**, `left: 10008` JUKEBOX against `right: 2` NOENT |

`git diff crates/cowfs-nfs/src` is empty after the restore.

## Lint, and what was not reproduced

Reported by the #19 lane as `cargo clippy -p cowfs-nfs --all-targets -- -D warnings` failing on
this expression under rustc 1.95 while 1.99 does not, on Linux, at `contract.rs:233`, in
`docs/verification/evidence/server-requirements19-portability.md` and `docs/verification/ready-19.md`.
The line is 238 on current main because 111 and 96 landed in between; it is the same expression.

**Not reproduced, and the lint name is not guessed.** The 1.95 toolchain is installed on this host
but its clippy component is not:

```
error: 'cargo-clippy' is not installed for the toolchain '1.95-aarch64-apple-darwin'.
```

Installing a component is outside this task, so the exact lint stays unreproduced.

| command | toolchain | result |
|---|---|---|
| `cargo +1.95 check -p cowfs-nfs --all-targets` | 1.95 | exit 0 |
| `cargo clippy -p cowfs-nfs --all-targets -- -D warnings` | 1.99 | exit 0 |
| `cargo fmt --all --check` | 1.99 | exit 0 |
| `cargo test -p cowfs-nfs` | 1.99 | 0 failures across every target |

The 1.95 `check` exiting 0 is the one thing established about the report: it is clippy-only and not
a rustc error. The 1.99 clippy result **neither reproduces nor refutes** the 1.95 report. The
expression the report names is gone, so whatever lint it raised can no longer fire.

## Reported, not fixed

`crates/cowfs-gc/tests/control.rs:75` has the same defect class,
`assert!(!f.gc_dir().join("mark.bin").exists() || true)`. It is outside this branch's ownership and
is not touched. A follow-up issue is warranted.

## Not claimed

No change to the #90 resolution. No production fix, no general quality refactor, no `cowfs-core`
metadata work, no mount-helper work. SIGKILL remains a process crash and not a power cut.
`docs/design.md` criterion 2, issue #88 and issue #17 all stay open.
