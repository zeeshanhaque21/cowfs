# PR 140 rustdoc correction: retracting a false doc-attachment claim and moving the `sync` block

Lane: READY5 follow-on correction for PR #140, branch `fix/meta-inode-reservation-42`.
Head this correction starts from: `2e6a31acd92c2418b90b664a23571a32bd8c648f`.
Scope: the existing #42 doc-placement defect only.
Nothing else is in this change.

This receipt **retracts one specific claim** made by the previous correction receipt
`9eb4fad0b7cc0e4920a08cbe85f5ffedbcd60b9b8690804daabcc0c37a1e1459`, and that claim was false.
The retraction is stated here in full rather than by silence.
The previous receipt is left byte-identical on disk and in git history, and so are the author's earlier
receipt `4ef55a3c` and the independent review `0f27f8bf`.
Where this receipt disagrees with `9ebfad0b`, this receipt is the newer statement.

## The retraction

The previous receipt, under "Fix 3: doc placement on the public surface", said:

> The fix is one line, a doc separator, which ends `sync`'s doc block and starts `reserve_inodes`'s.

and then:

> `crates/cowfs-meta/src/db.rs:1617-1620`. The public documentation of `Meta::sync` is restored,
> `Meta::reserve_inodes` keeps only its own accurate paragraphs, and the nine-line self-contradiction is
> gone.

**Both sentences are false, and the change did nothing at all to doc attachment.**

A blank `///` line does not terminate a rustdoc comment.
Every consecutive `///` line above an item, including a blank one, is one single doc comment attached to
the item that follows it.
Adding a blank `///` therefore inserted a blank **paragraph** into one already-contiguous block and
changed no attachment whatsoever.

The coordinator's actual diff of `f5f7bbc8..2e6a31a` on `db.rs` confirms it independently: the entire
change is one added blank `///` line, `1 file changed, 1 insertion(+)`.

## Proof of the defect, by direct source attachment analysis

The rule mirrored below is the rustc rule: a doc comment is the contiguous run of `///` lines
immediately preceding an item.
A blank `///` line belongs to that run.
A blank source line, or any line that is not a `///` comment, ends the run.

Applied to the committed `2e6a31a`:

```
pub fn reserve_inodes(&self, n: u64) -> Result<InoRange> {   [line 1637]
  attached block: lines 1617-1636   20 lines, one contiguous run
  carries sync text   : true
  carries reserv text : true

pub fn sync(&self) -> Result<()> {   [line 1641]
  attached block: (none)
  carries sync text   : false
  carries reserv text : false
```

So at `2e6a31a` the public `Meta::reserve_inodes` still carried `Meta::sync`'s paragraph, and the public
`Meta::sync` was still undocumented.
The defect the independent review `0f27f8bf` found was **still open** at that head, and my previous
receipt declared it repaired.
That is the error this receipt exists to correct.

## The actual correction: move the `sync` block

Because a separator cannot work, the `sync`-specific block is **moved** so that it is directly above the
only item it describes, and the reservation block is left directly above `reserve_inodes`.

`crates/cowfs-meta/src/db.rs`, three lines removed and three lines added, nothing else:

```diff
@@ -1614,9 +1614,6 @@ impl Meta {
-    /// Runs `before_sync`, then makes every applied change durable. The hook runs on every call,
-    /// also when nothing is pending, so a caller can use this as "sync the store, then the
-    /// metadata". Returns the hook's or the commit's error.
     /// Reserves `n` inode numbers before any inode exists, and hands them back.
     ///
     /// The numbers come from the same allocator [`Snapshot::batch`] creation draws on, so an
@@ -1637,6 +1634,9 @@ impl Meta {
+    /// Runs `before_sync`, then makes every applied change durable. The hook runs on every call,
+    /// also when nothing is pending, so a caller can use this as "sync the store, then the
+    /// metadata". Returns the hook's or the commit's error.
     pub fn sync(&self) -> Result<()> {
         self.h.inner.sync()
     }
```

No blank separator line was added anywhere.
The text moved verbatim, with no rewording, no reordering inside the block, and no change to any other
doc comment in the file.

## Proof of the corrected association, for every method, with exact positions

The same attachment analysis, over the whole `impl Meta` public surface, at the corrected working tree:

```
pub fn health(&self) -> Health                       [line 1613]
  attached doc block: lines 1608-1612, 5 lines, one contiguous run
pub fn reserve_inodes(&self, n: u64)                [line 1633]
  attached doc block: lines 1617-1632, 16 lines, one contiguous run
  carries sync text   : false
  carries reserv text : true
pub fn sync(&self) -> Result<()>                     [line 1640]
  attached doc block: lines 1637-1639, 3 lines, one contiguous run
  carries sync text   : true
  carries reserv text : false
pub fn close(&self) -> Result<()>                    [line 1647]
  attached doc block: lines 1644-1646, 3 lines, one contiguous run
```

The two blocks now in question, verbatim from the source at their exact lines:

`Meta::reserve_inodes`, `db.rs:1617-1632`, sixteen `///` lines, attached to the item at `db.rs:1633`:

```
1617 /// Reserves `n` inode numbers before any inode exists, and hands them back.
1618 ///
1619 /// The numbers come from the same allocator [`Snapshot::batch`] creation draws on, so an
1620 /// ordinary create never receives one of them and this never receives one from a create.
1621 /// Numbers are contiguous and `end` is exclusive.
1622 ///
1623 /// The durable floor is committed before this returns, so a number is never reissued after a
1624 /// reopen, including one that was reserved and then never used. Because it commits, it is a
1625 /// durable operation rather than an applied one: it runs under the same lock as a batch and
1626 /// runs no `before_sync` hook, since it carries no chunk references.
1627 ///
1628 /// Asking for zero is [`Error::Invalid`], and asking for more than the remaining numbers below
1629 /// [`INO_LIMIT`] is [`Error::LimitExceeded`]. Neither writes anything.
1630 ///
1631 /// This hands out numbers; it does not create inodes. Creating an inode at a reserved number is
1632 /// a separate concern and is not provided here.
1633 pub fn reserve_inodes(&self, n: u64) -> Result<InoRange> {
```

`Meta::sync`, `db.rs:1637-1639`, three `///` lines, attached to the item at `db.rs:1640`:

```
1635 }
1636
1637 /// Runs `before_sync`, then makes every applied change durable. The hook runs on every call,
1638 /// also when nothing is pending, so a caller can use this as "sync the store, then the
1639 /// metadata". Returns the hook's or the commit's error.
1640 pub fn sync(&self) -> Result<()> {
1641     self.h.inner.sync()
1642 }
```

The two original defects are both gone, on the public surface, by attachment and not by intention:

- `Meta::reserve_inodes` no longer inherits a paragraph claiming it runs `before_sync` and makes applied
  changes durable, which it does not do, and which its own later paragraph contradicted.
- `Meta::sync` is documented again, with the text that was always meant for it.

## What kind of proof this is, stated precisely

It is a **source-structural argument about which comment run attaches to which item**, computed from the
file bytes by a walk that mirrors the rustc rule, plus a read of the source.
It is **not** rustdoc output and it is **not** a compiled documentation check.

No AST metadata parser was available to cross-check it.
`rustdoc` and `rust-analyzer` are both installed, but both resolve the crate's modules and its external
dependencies before they can report on doc attachment, which means a Cargo build.
Cargo is forbidden on this lane, so neither was run, and neither was forced.

The only executable check on this lane was the standalone formatter:

```
$ rustfmt --edition 2021 --check crates/cowfs-meta/src/db.rs
rustfmt exit=0
```

**That is a formatter result and nothing more.**
It is not evidence about doc attachment, and it is not presented as such anywhere above.
The formatter result is reported because it is the one check available and it passed, not because it
proves the correction.

## This change is documentation only

Every changed line is a `///` line.
Verified against the pre-correction head `2e6a31a`, the whole diff across the repository is:

```
crates/cowfs-meta/src/db.rs | 3 insertions(+), 4 deletions(-)
```

The fourth deletion is the blank `///` line that the previous, ineffective change had added.
Against the original reviewed head `f5f7bbc8`, the diff is `3 insertions(+), 3 deletions(-)`, which is the
same three lines relocated and nothing else.

No signature, no statement, no expression, no test, no format, no dependency and no file under
`crates/cowfs-core` was touched.
The reservation loop is byte-identical to `2e6a31a`.

The test file is byte-identical to `2e6a31a`, verified by digest, so the two lint corrections stand
untouched and no test needed any change in this lane:

```
5ed9cd3b7a5680cdf77eaf6a440471d05b235448eb686057fc2749bb6aa300ce  crates/cowfs-meta/tests/inode_reservation.rs
```

## The count-cap proposal is still a proposal and is not approved

Receipt `9ebfad0b` proposed bounding a reservation to one inode block per call so the write-lock hold
stops scaling with `n`.
**That proposal is not approved, is not implemented, and its contract narrowing is not agreed.**

It is restated here only to prevent the doc correction being read as progress on it.
It narrows an arbitrary-count public contract to a bounded-count contract, and it must be reviewed
against the actual `record_recovery` semantics at `db.rs:765-800` and against an executed
commit-failure proof before anything is implemented.
Neither has happened.
The commit-failure behaviour remains a source-argued gap, because `reserve_durable` still has no
injectable seam, and no seam has been added.

No `Core` migration, no new public consumption API, no new issue and no new task were created here.
Issue #42 stays **open** and is not claimed as done in any part.
No claim is made anywhere in this receipt that #42 or request 4 is complete.

## Lane and cap discipline

No cargo build, test, clippy, `cargo doc`, archive, probe, target directory or archive was run.
No deletion, move, offload or cap waiver, and nothing written under `bench/out`.
`bench/out` remains at 20.863 GiB against an 8 GiB cap with the protected `ready-40` unreachable, which
is why no build ran here.
No lease was taken, returned or altered, and no checkout, reset, rebase or stash was performed.
No signal, restart, install, `sudo`, mount walk, store, socket or job was touched.
MisakaNet was consulted local only and returned nothing applicable to a rustdoc comment-attachment
defect.

The metadata path owned by this lane is `crates/cowfs-meta/src/db.rs` only.
`crates/cowfs-meta/tests/inode_reservation.rs` was read for digest verification and was not modified.
Other lanes in flight on PR #141, PR #139 and the clock work were not read, modified or reverted, and
their files and artifacts are untouched.

Refs #42 request 4.