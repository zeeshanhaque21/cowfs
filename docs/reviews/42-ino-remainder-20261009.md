# Issue 42 remainder: virt.ino and the alias table (2026-10-09)

Scope: request (b) of the triage, retiring `virt.ino` and the alias table in cowfs-core.
Status: not done in this change, because it is not small and it touches public API.

## What is already true on main

Creates already consume `Meta::reserve_inodes`.
`crates/cowfs-core/src/ns.rs` takes a reserved ticket at the create (`take_reserved`), and the number a client sees is the packed meta number.
`crates/cowfs-core/tests/reserved_inode_identity.rs` pins that identity across flush and reopen.
`crates/cowfs-core/src/lib.rs` at the open path says nothing hands out virtual numbers any more.
So the issue's functional ask is met: no production path mints a `VIRT` number.

## What is left

1. `crates/cowfs-core/src/ino.rs`: `VIRT`, `MAX_VIRT_SNAP`, `Id::Virt`, `virt()`, the mark reader `read_virt_mark` with `Mark::counter`, and the `Aliases` table.
2. `crates/cowfs-core/src/inner.rs`: `Id::Virt` arms in `meta_of`, `canon` and the node loader, plus `alias_limit` in `Options`, `aliases_dropped` and `aliases` in the stats.
3. `crates/cowfs-core/src/lib.rs`: the `Core::open` read of the mark (`ino::read_virt_mark`), the `alias_table` test seam, the `VIRT_COUNTER_MASK` re-export, `purge_snapshot` on remove.
4. Tests that exercise the alias shapes: `tests/alias.rs`, `tests/alias_session.rs`, `tests/names_ino.rs` and parts of the ino unit tests.

## Why it is not bundled here

The aliases still serve a purpose for numbers handed out by an earlier release: `Aliases::insert` keeps a forward entry so `meta_of` answers for a legacy virtual number held by a client.
Removing the table is a compatibility decision (what does a client holding a legacy virtual number get after upgrade, `Stale`?), not a cleanup.
`Options::alias_limit` and the `aliases` stats field are public, so removing them changes the daemon and the API surface.
The mark files (`virt.ino.a`, `virt.ino.b`) are an on-disk artifact the open path still reads; deleting the read changes what an old store does on open.

## What it needs

1. A decision: legacy virtual numbers return `Stale` after the upgrade, or the table stays for one release with a deprecation note.
2. Remove `Id::Virt` and the alias table, and make `classify` treat a top-bit number as `Stale` input.
3. Stop reading the mark files at open, and remove them from a store on the first open after the upgrade (a one-shot, logged, idempotent cleanup).
4. Drop `alias_limit`, `aliases_dropped`, `aliases` and the `alias_table` seam, and update the daemon stats consumers and docs/v1-core.md "Inode numbers".
5. Port the alias tests to assert the stale behaviour, and keep `reserved_inode_identity.rs` as the identity gate.
6. Failing-first: a test that opens a store with a legacy mark and a client-held virtual number, before and after.

## Related, not part of it

A second window remains in a promote that replaces an existing target: removing the victim and renaming the staged tree are two metadata commits, with the intent file covering the gap.
Closing it needs a `Meta` API that replaces a snapshot by name in one transaction (decision D13), which is outside cowfs-core.
