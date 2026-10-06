# #42 request 4: Core reserved-inode control-test correction

Status: test correction committed and pushed.
Runtime UNEXECUTED on this head because a full-worktree capacity block forbids local cargo, build, and test.
This receipt records a correction to the *negative control* in the #42 request 4 regression test, and the positive-case proof it must not contradict.

## Why this correction exists

An independent review (the e7d review) called the third test `a_number_from_a_closed_session_is_stale_after_a_reopen` a valid control.
That call was wrong: the review did not check the control against the positive case, and the two contradict each other on the NEW design.

The contradiction, in the corrected file's own previous version at HEAD `399153a`:

- Positive case, `crates/cowfs-core/tests/reserved_inode_identity.rs` lines 113-116 assert the created number and the reopened number are equal:

  ```rust
  assert_eq!(
      a.ino, created,
      "the file's inode number changed across a reopen: {report}"
  );
  ```

- Old third control, same file lines 164-181, saved the *created* number `a.ino` as `stale`, reopened, and asserted `getattr(stale) == Err(Error::Stale)`.

On a real reservation design the created number is durable, so the positive case requires `created == reopened` and a fresh `getattr` on it must *succeed*.
The old control required a fresh `getattr` on a created number to return `Stale`.
Those cannot both hold on the NEW design.
The old control passed on OLD only because OLD hands out a session-local virtual number, which is exactly the bug the positive case locks down.

## The fix

Rename the control to `a_virtual_alias_number_is_stale_after_a_reopen`, and make it test the alias *shape* rather than any created file's real identity.
The control number is now tagged virtual on purpose:

```rust
// The legacy alias shape: top bit set, tagged virtual on purpose. On a
// real reservation this number is never handed out, so it stays stale.
a.ino | VIRT
```

The test asserts the tagged number really is alias-shaped (`& VIRT != 0`), then that `getattr` on it after a fresh reopen is `Err(Error::Stale)`.

## Why the tagged number is meaningful and cannot be a real admitted ID

`crates/cowfs-core/src/ino.rs` defines the shapes:

- `VIRT = 1 << 63`, and `ROOT_INO = 1`.
- `classify`: `ino == ROOT_INO` is `Root`; `ino & VIRT != 0` is `Virt`; otherwise `Meta`.
- A real meta-derived number is `pack(snap, m)` with `snap < MAX_SNAP` and `m` in the low 40 bits, so it never sets bit 63.

A real, reservation-backed number therefore never has the `VIRT` bit, so `a.ino | VIRT` is a distinct, non-admitted identity by construction.
It cannot collide with a real admitted number, and it cannot be the root, because the root is `1` and the tagged number has bit 63 set.

Resolution is also deterministic. `crates/cowfs-core/src/vfs_impl.rs` sends `getattr` to `Inner::op_getattr`, which calls `live(ino)` and then `load_node`.
In `crates/cowfs-core/src/inner.rs` `load_node`, the `Id::Virt` arm is:

```rust
Id::Virt { snap } => (snap, self.aliases.rd().meta_of(ino).ok_or(Error::Stale)?),
```

A VIRT-tagged number with no alias entry returns `Error::Stale` before any meta read.
A fresh session has no alias entry for a number it never handed out, so the control is honest and stable.

Scope note: this proves the legacy alias shape is refused after a reopen.
It is not a proof about migrating an actual OLD store, and it does not claim to be.

## Byte-identical positive fixture

The two load-bearing positive tests, their assertions, the `report` gathering, and the byte path are unchanged.
The test-file bytes at HEAD `399153a` still hash to `e5ded873aa9c1b43c9d87a977aaab05a3a504a6a58118d7cf8c07e2ba124ce40`, which is the value recorded in the prior receipt `meta42-core-reserved-inode-regression.md`.
The correction touches only the third control; the positive fixture commit is byte-identical.

## Expected shape of the corrected control

On OLD, the corrected control still passes: `a.ino` is already virtual, so `a.ino | VIRT == a.ino`, the alias-shaped assertion holds, and a fresh `getattr` is `Stale`.
On NEW, a created number is durable and un-tagged, so `a.ino | VIRT` tags a distinct number that was never admitted; `getattr` on that tagged number is `Stale`, while the positive case's created number resolves normally.
The control therefore keeps its meaning under both designs without contradicting the durable-identity positive case.

## Runtime status

UNEXECUTED on the corrected head, by the capacity block in force at correction time (no local cargo, build, test, or archive).
The observed earlier runtime result `1 passed; 2 failed` belongs to the old fixture at HEAD `399153a` only.
That earlier run had the wrong control: it passed for the wrong reason.
The corrected head carries the positive fixture unchanged and an unexecuted control.
`rustfmt --edition 2021 --check` on the test file exits 0.

## Provenance

- Test file: `crates/cowfs-core/tests/reserved_inode_identity.rs`.
- Corrected test source SHA-256: `583032cfd4164e789785dd89fd3e3bc2dc4a4ea5482cd83e4013e5aa041402a2`.
- Prior test source SHA-256 (at HEAD `399153a`, positive fixture byte-identical): `e5ded873aa9c1b43c9d87a977aaab05a3a504a6a58118d7cf8c07e2ba124ce40`.
- Base and prior head: `399153a6e69a3d2a292a9ccb42291221f81b6dfd`, branch `fix/core-reserved-inode-consumer-42`.
- No production Core or Meta code, schema, or selected-ID API was changed.
- The failed independent review is not rewritten; this receipt supersedes its negative-control claim only.
