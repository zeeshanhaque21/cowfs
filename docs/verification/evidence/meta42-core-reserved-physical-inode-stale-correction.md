# #42 Core: reserved physical inode `Stale` correction

Head: `6370060209779d25c04c89eda0036f42a4e272d0` (PR 142, draft, Refs #42).
Prior head: `2a8b83d77a2c20035121e36e0532648a6f139fd0`.
Parent of prior head: `6a0515e6460b9211d8cbf51164b8a1b8ea6960bc`.
Merge base: `e488a17b67b30b31be6f3f19f6a9e0e6aa8c94e`; main: `1580e69b9d987f63c07b2430f8c0b4547ecd8622`.

## What this correction fixes

The delivered `2a8b83d` fixed the reverse self-map only; that fix is preserved.
The 26 conformance `Stale` failures were masked before it because the conformance
binary aborted on a stack overflow (a cycle from the self-map), so it never ran.
They are a feature-wide defect in the reservation-backed create path, not in the
`2a8b83d` guard.

Root cause, proven by main-vs-branch source comparison: on main `make` assigned a
virtual number, so `classify(ino) = Id::Virt` and `meta_of` returned `None` until
the create committed, and every meta reader degraded gracefully. On the branch
`make` assigns the packed meta number, so `classify(ino) = Id::Meta{m}` and
`meta_of` returns `Some(m)` unconditionally, even before meta has committed the
create. Every reader that then consults meta for a not-yet-committed inode gets
meta `NotFound`, mapped to `Stale`. `committed_meta`, used by `op_setxattr`, could
not detect this because its predicate was "`meta_of` is `None`", never true for a
physical number.

## The fix (3 lock-safe seams)

1. `ns.rs` `make`: record the create's sequence on the child (`node.ns_seq`), so the
   existing `op_readdir` / `require_empty` / `barrier_if_needed` gates commit a
   pending child create before reading meta through it.
2. `io.rs` `committed_meta`: predicate on the node's own pending-create signal
   (`node.seq` vs `sc.flushed`) instead of on `meta_of` presence. Covers
   `op_getxattr`, `op_listxattr`, `op_removexattr`, `op_setxattr`; `op_setxattr`'s
   dead `xattr_exists` helper removed.
3. `inner.rs` `preserve_orphan`: skip the meta read for an uncommitted create. Meta
   has no xattrs for it yet, and callers hold the namespace lock, so a barrier
   cannot run there; an empty map is materialized instead. Self-safe for all four
   call sites (`op_unlink`, `op_rmdir`, `op_rename` non-dir overwrite, `rename_dir`).

## Changed paths (worktree `crates/`, worktree mirror only)

- `crates/cowfs-core/src/ns.rs` (sha256 `bfd5366a1625fc89dafa130c633dce8c26c7737c87544b79df2e417469088dc4`)
- `crates/cowfs-core/src/io.rs` (sha256 `42d1f550c0c5aeee2b83e88edf6d69577046b206562ca19e29c6b05f15d99510`)
- `crates/cowfs-core/src/inner.rs` (sha256 `c30a423cff70f7a1601f10599e2628e1074174a3d52b58c9d6bba0ff294bbda6`)
- `crates/cowfs-core/tests/reserved_inode_metadata.rs` (new; sha256 `169f47d3b7fed94229c59dd5c191bd83fa3c8f73172f6e1155a4037f674175d7`)

## Evidence status

- Local `cargo build` / `cargo test`: UNEXECUTED (resource binding). No runtime
  result is claimed for this head.
- `rustfmt --check --edition 2021` on all four owned files: PASS.
- New CI on head `6370060`: PENDING (no workflow run exists for this head yet; not
  dispatched, not polled).
- Prior head run `37532795484` completed `failure`: conformance 197 pass / 26 fail,
  every failure `unexpected error: stale inode (Stale)`; alias unit suite green;
  `a_create_past_the_alias_ceiling_is_refused` and
  `a_session_alias_costs_a_bounded_number_of_bytes_per_inode` green.

## Scope honesty

This correction is not yet runtime-verified on `6370060`. The alias delivery
(`2a8b83d`) is independently confirmed. The whole-#42 acceptance is not claimed:
the 26 conformance checks and the broader #42 runtime gates await a CI run on this
head. No old conformance expected result was changed, no alias was zeroed, no
virtual-number reversion, no user batch cap.