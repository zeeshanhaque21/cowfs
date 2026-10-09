# PR 142 independent review: Core reserved-inode consumer RED fixture

Reviewer: wbuddy.
Scope: read-only source review of the RED test fixture only.
PR: #142, draft, head pinned `399153a6e69a3d2a292a9ccb42291221f81b6dfd`.
Base pinned: `89353e17e5085000711dc428e834f9cc41840a1f`.
Runtime: UNEXECUTED by this reviewer. Every claim is a source read of the pinned blobs via `git show`. No build, test, or run was performed here.

## Verdict

SOURCE PASS on the fixture.
The fixture is a legitimate TEST-FIRST regression: it fails on the pinned base for a real identity assertion, not a compile error, not a missing-API name, not a workaround.
It is a RED WIP and is not mergeable and claims no delivery.

No NEW PASS and no acceptance is claimed by this review.
Two of the three cases fail on the pinned base by design; the third passes on the base as a documented negative control.

## Pin verification

- Remote `refs/pull/142/head` is exactly `399153a6e69a3d2a292a9ccb42291221f81b6dfd`. Confirmed against the real remote.
- Remote `refs/heads/main` is exactly `89353e17e5085000711dc428e834f9cc41840a1f`, which is also the PR parent (`399153a6` -> `89353e1`). Confirmed.
- PR diffs one new file only: `crates/cowfs-core/tests/reserved_inode_identity.rs`, 181 insertions, no production file touched.
- Test blob SHA-256 at the pinned head is `e5ded873aa9c1b43c9d87a977aaab05a3a504a6a58118d7cf8c07e2ba124ce40`, which matches the author-reported value exactly.
- Coordinator report hash matches the reported `10b62c06cdfeef54986c8b2e49e3d6935e0b32bc71c4cd07b535bd146b4aee24` (94 lines).

## Test source and provenance

- File: `crates/cowfs-core/tests/reserved_inode_identity.rs` (181 lines).
- Three cases: `a_created_file_keeps_one_durable_identity_across_a_flush_and_reopen` (main, RED on base), `a_created_number_is_never_the_virtual_alias_shape_or_the_root` (negative control, RED on base), `a_number_from_a_closed_session_is_stale_after_a_reopen` (negative control, PASS on base).
- Uses only existing public Core API: `Core::open`, `create_snapshot`, `create`, `lookup`, `getattr`, `sync`, `check`, and the existing `#[doc(hidden)]` `Core::meta_inode` seam plus `Vfs::flush`. No `reserve_inodes`, `InoRange`, `InoTicket`, or `create_at` name appears, so an old-fail cannot be a missing-name compile error.
- Author-reported local run: `1 passed; 2 failed`, exit 101, on branch `fix/core-reserved-inode-consumer-42` at base `89353e17`. This reviewer did not rerun it (see Runtime).

## Assertion validity

The main case gathers all four observations first, then asserts, so the firing assertion carries the full `report` string. Source-checked clause by clause:

1. `created & VIRT == 0` (line 102). On the base, `ns::make` calls `alloc_virt` (`ns.rs:177`), which returns `ino::virt(snap, n)` with the top bit `VIRT = 1 << 63` set (`ino.rs:63-67`). So `created` has the `VIRT` bit and this assertion fails on the base. Valid.
2. `meta_before.is_some()` (line 108). `Core::meta_inode` calls `Inner::meta_of` (`inner.rs:300`), which classifies the number. A virtual number takes the `Id::Virt` branch and reads the alias `fwd` map (`ino.rs:84`), which is only populated at commit time (`inner.rs` `commit` inserts into a cloned alias map on success). So on the base `meta_before` is `None`. Valid and consistent with the reported evidence.
3. `a.ino == created` (line 113) and `meta_after_reopen == meta_after_flush` (line 117). On the base the fresh session resolves the file through `pack(snap, m)` to the packed meta number (`0x10000000002`), which differs from the virtual `created` (`0x8000010000000001`). Valid.
4. `read_all(&c, a.ino) == bytes` (line 123) and `c.check()` (line 124), the byte-and-invariant tail. Valid.

### Is `meta_before.is_some()` a valid expectation for the desired reserved-ID path?

Yes, and it does not force an early create transaction.
On the desired path the caller holds a non-virtual number, so `classify` yields `Id::Meta { m }` and `meta_of` returns `Some(m)` directly from the number's own bits (`inner.rs:301`); it needs neither the alias table nor a committed inode record nor a reopened meta handle.
The producer-only reservation semantics are preserved: the reservation makes the number a real meta number, and assertion 2 becomes true when the create path hands back that reserved number instead of a virtual alias.
So the clause is a correct statement of the contract, not an accidental demand for a create-time commit.

## Quiescence and raw-ID correctness

- `test_opts()` sets `background: false` (`tests/common/mod.rs:34`), so no background flusher races the reopen. The timing hazard is designed out.
- The first session is scoped in a block that drops before the fresh `Core::open`, and `Vfs::flush` plus `c.sync()` make the file durable first. The reopen reads a quiesced store.
- Comparison is on full `u64` values and `Option<u64>` values; there is no truncating or unsafe cast and no negated-comparison vacuity. `VIRT` is restated as a local constant because the test cannot reach the crate-private constant; the bit value is the same (`ino.rs:10`).

## Focused challenge per load-bearing boundary

- Copy/replay: the fixture does not exercise the queue replay path; that boundary belongs to the future consumer implementation, not this fixture. Not a fixture defect.
- Wrong store: not exercised here.
- Reopen: exercised correctly by the drop-then-reopen structure with `background: false`.
The fixture is deliberately narrow, which is correct for a RED contract test.

## Counts and honesty

- Positive case: 1. Negative controls: 2. Ignored tests: 0. `#[ignore]` absent.
- The report states `1 passed; 2 failed` with the exact assertion text and the four observed values, and the observed values match the pinned source semantics.
- The report expressly says neither this tree nor `main` contains the reservation consumer API, so the old-fail is a real identity assertion rather than a compile error. This matches the source (no `reserve_inodes` caller exists on the base).

## CAP audit (operational)

The task required a full READY3 artifact inventory and a projection against a `<= 8 GiB` cap and a free-space floor before any run.

- READY3 tree: `/Users/zeeshanhaque/Projects/cowfs/.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/3/cowfs`.
- Measured total (read-only `du -sh`): approximately 7.6 GiB.
- Of that, `target/` is approximately 456 MiB.
- Host free space at the mount: approximately 227 GiB available (88 percent used). The free-space floor is satisfied.
- No preflight or cap-proof log was found under the READY3 tree (`find` for `*preflight*` / `*cap*` returned nothing).

Assessment: the tree is already close to the `<= 8 GiB` cap with only the 456 MiB `target/` counted; a from-scratch recompile plus test run would grow `target/` well past that figure. The author report quotes only the target size, not the full tree, so the full-cap-and-projection proof was not established in the report.
This is an operational BLOCK on any further heavy or batch work in this lane, as the task specified. It does not change the source verdict on the fixture.

## CI status (one read, no polling)

Read once via `gh-axi pr checks 142`:

- `check (ubuntu-latest)`: fail
- `check (macos-latest)`: pending
- `linux-fuse`: pending
- summary: 0 passed, 1 failed, 2 pending, 3 total.

The ubuntu failure is the expected RED of the test-first fixture on the base (the test is authored to fail on the unmodified tree), not evidence of a compile break. This reviewer did not rerun, retry, or dispatch CI. No conclusion beyond what the one read shows.

## Scoped verdict

- Verdict: SOURCE PASS on the fixture.
- Test named: `a_created_file_keeps_one_durable_identity_across_a_flush_and_reopen`, with two negative controls; 2 fail, 1 pass on the base.
- Source binding: test blob SHA-256 `e5ded873aa9c1b43c9d87a977aaab05a3a504a6a58118d7cf8c07e2ba124ce40` at head `399153a6e69a3d2a292a9ccb42291221f81b6dfd`, base `89353e17e5085000711dc428e834f9cc41840a1f`.
- No NEW PASS and no acceptance claimed: this is a RED WIP, not mergeable, delivery not claimed.
- CAP: full artifact inventory measured at about 7.6 GiB against an 8 GiB cap, target about 456 MiB; free space about 227 GiB. Full-cap projection not proven in the author report. Operational BLOCK on further heavy work.
- Runtime: UNEXECUTED by this reviewer. Source reads only.

## Notes held for the consumer implementation, not this fixture

- The prior consumer plans (`meta42-core-inode-consumer-plan.md`, `meta42-core-inode-consumer-safety-correction.md`) remain PROPOSALS, not approved, and their `InoTicket` numeric-membership design cannot reject a colliding store-A/store-B ticket by number alone. The queue and numeric-identity authority gaps remain open for the actual consumer implementation.
- Those design questions are out of scope of this fixture review and are not re-litigated here.
