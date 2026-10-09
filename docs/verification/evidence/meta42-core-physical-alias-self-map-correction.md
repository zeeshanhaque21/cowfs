# #42 request 4: the reserved-create reverse self-map, and a receipt correction

Status: source fix pushed on draft PR #142, head `2a8b83d77a2c20035121e36e0532648a6f139fd0`.
CI run `37532795484` completed `failure` on two unrelated checks, see "What this fix did and did not change".
The narrow alias fix itself passes its target test and the unit suite.

This receipt corrects two false statements in
`docs/verification/evidence/meta42-core-alias-contract-and-pending-retry-correction.md`
(sha256 `271205e5891127eb38c75e4ef2d99d1bc06cf21865339aa1e01b76b0067bd4c9`), which is preserved unedited.
It does not restate the alias-contract fix or the retry repair.

## Correction 1: the reserved alias IS a reverse self-map

The historical receipt, lines 45 to 47, states:

> The guard's comment claimed a reserved alias would be a "self-entry". It is not: the alias key is
> the packed visible number and the value is the bare meta number, so `meta_of` and `canon` both
> resolve through it correctly.

That statement was the branch author's rationalization for deleting the guard, and it is false in the reverse direction.

The forward and reverse maps are not symmetric.
`Aliases::insert(virt, snap, m)` writes `fwd[virt] = m` and `rev[pack(snap, m)] = virt`.
For a reservation-backed create, `make` sets `ino = pack(snap, ticket.ino().0)` (`crates/cowfs-core/src/ns.rs`), so `virt == pack(snap, m)`.
The forward entry is therefore fine, but the reverse entry is `rev[pm] = pm`, a self-map.

`Inner::load_node` reads `canon` for an `Id::Meta` number and reloads through its result:
`self.nodes.get(&v).map_or_else(|| self.node(v), Ok)`.
With `v == ino`, an uncached node recurses into `load_node` with the same argument until the stack is exhausted.

So the guard's comment was right about the reverse direction and the historical receipt was wrong to call it "not a self-map".
The self-map is real and it is the cause of the pre-fix abort.

## Correction 2: run `37532881115` does not exist

The historical receipt, line 99, cites "Run `37532881115` is the latest".
`gh api repos/zeeshanhaque21/cowfs/actions/runs/37532881115` returns HTTP 404.
That run id was never created; the citation is fabricated or mistyped.

The real runs on the branch, newest last, as returned by `gh-axi run list --branch fix/core-reserved-inode-consumer-42`:

| run id | head | conclusion | what it shows |
| --- | --- | --- | --- |
| `37527891397` | `67fd0ac` | failure | alias regression, pre-fix |
| `37528269132` | `67fd0ac` | failure | alias regression, pre-fix |
| `37530284116` | `b588a0e` | failure | meta test compile error (`Meta::getattr`) |
| `37530637925` | `6a0515e` | failure | `fatal runtime error: stack overflow, aborting` |
| `37532795484` | `2a8b83d` | failure | alias tests pass; conformance `Stale` failures surface |

The only run on head `6a0515e` is `37530637925`, and it aborted with a stack overflow.
There is no green run on any pre-fix head.

## The runtime abort, reproduced from CI logs

Pre-fix run `37530637925`, both `check` jobs (ubuntu job `112498834377`, macos job `112498834519`):

```
test result: ok. 30 passed; 0 failed; ... finished in 0.64s
fatal runtime error: stack overflow, aborting
```

The abort lands immediately after the `cowfs-core` unit-test binary and before the `alias`, `reserved_inode_identity`, and `conformance` integration binaries.
Those integration binaries never ran on the pre-fix head, which is why the alias acceptance test reported an abort rather than an assertion, and why the conformance failures were invisible.

## The fix

`crates/cowfs-core/src/ino.rs`, `Aliases::insert`: write the reverse entry only when `pm != virt`.

```rust
self.fwd.insert(virt, m);
if let Ok(pm) = pack(snap, m) {
    if pm != virt {
        self.rev.insert(pm, virt);
    }
}
```

Effect, all four deliberate:

- The forward entry is unchanged, so the alias count, the session ceiling, and `meta_of` are unchanged.
- `canon(snap, m)` now misses for a physical self-alias.
- `Inner::load_node` then takes `(snap, m)` and loads at `pack(snap, m)`, which is the same number, so behavior is identical for a genuine create.
- A genuine virtual bridge (`virt != pack(snap, m)`) still writes its reverse entry, so the legacy virtual path is unbroken.

Two unit tests in `ino.rs` pin both directions:
`a_physical_self_alias_does_not_create_a_reverse_self_map` and `a_physical_self_alias_does_not_disturb_a_virtual_bridge`.

No fake counter, no alias zeroing, no return to virtual ids, no recursion guard.
The fix removes the malformed map entry rather than papering over the reload.

## What this fix did and did not change

Did change: the pre-fix stack-overflow abort is gone.
Post-fix run `37532795484` runs the whole `cowfs-core` suite to completion, and the two alias acceptance tests pass:

```
test a_create_past_the_alias_ceiling_is_refused ... ok
test a_session_alias_costs_a_bounded_number_of_bytes_per_inode ... ok
```

Did not change, and did not cause: the two `check` jobs still fail on `cowfs-core --test conformance`, `106 passed; 26 failed`, every failure `unexpected error: stale inode (Stale)`, including `inode_numbers_are_never_reused`, `stale_after_reclaim_for_every_operation`, and `rmdir_updates_parent`.

That failure is not this seam.
This commit touches only the reverse-map insertion in `ino.rs` and adds two tests.
It cannot change the number `make` returns, the snapshot-context resolution, or when a node is considered stale.

The failure is pre-existing branch behavior that the abort had been masking.
Evidence, not theory:

- Pre-fix run `37530637925` aborted before the conformance binary ever executed, so conformance had no result on the branch.
- Main `1580e69b9d987f63c07b2430f8c0b4547ecd8622`, which lacks the reserved-create path, is green including conformance (run `37530361261`).
- The branch's reserved-create path hands out the physical packed number `pack(snap, ticket.ino().0)` (`ns.rs`), a design change introduced in `b588a0e` and refined in `67fd0ac`, not here.

This is reported as a separate finding against the branch, not fixed under this narrow alias task.
Scoping it in here would be a different change than the one the review asked for.

## Verification done and not done

Done:

- Source-level root cause of the abort, traced through `make` -> `commit_batch` -> `Aliases::insert` -> `canon` -> `load_node`.
- Runtime confirmation from CI logs: pre-fix abort, post-fix pass on the target test and unit suite.
- Located the exact false claims in the historical receipt (lines 45-47 and 99) and disproved both.
- Confirmed run `37532881115` is HTTP 404 and listed the real runs.
- Confirmed the review doc sha256 `d3e917aa517a4907c10d2513b53f585a85f835ad2612fa78326ee9f0f520be11` still matches; it is unchanged.
- `rustfmt --edition 2021 --check` exit 0 on `crates/cowfs-core/src/ino.rs`.

Not done:

- No local `cargo build` or `cargo test`: the resource binding forbids a local heavy run.
- Full-workspace CI is still red on the pre-existing conformance regression, which is out of this task's scope.
- Whole-#42 acceptance is not claimed.

## Historical record

The file `docs/verification/evidence/meta42-core-alias-contract-and-pending-retry-correction.md` is preserved byte-for-byte (sha256 `271205e5891127eb38c75e4ef2d99d1bc06cf21865339aa1e01b76b0067bd4c9`).
Its two false claims are corrected above rather than edited in place, so the record of what was believed at the time stays intact.

## Head

- `2a8b83d77a2c20035121e36e0532648a6f139fd0`, branch `fix/core-reserved-inode-consumer-42`, pushed over HTTPS.
- Parent: `6a0515e6460b9211d8cbf51164b8a1b8ea6960bc`.
- Changed paths in this commit: `crates/cowfs-core/src/ino.rs`.
