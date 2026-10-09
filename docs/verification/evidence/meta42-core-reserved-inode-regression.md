# #42 request 4: Core reserved-inode consumer regression (test-first)

Status: test-first regression authored and failing on the current tree.
This receipt is the canonical record for the NEW regression test.
It is written before any production change, so it is a proof of the OLD behavior and a contract the fix must satisfy.

## What this covers

Issue #42 request 4 is the Core consumer side of inode reservation.
PR140 adds `Meta::reserve_inodes(n)`, which hands out inode numbers without creating inodes.
The consumer contract is that a file created at a number drawn from such a reservation keeps that same durable number, and that the number survives a flush and a reopen with the same identity and the same bytes.

This test pins that contract using the existing public Core API only.
It does not reference `reserve_inodes`, `InoRange`, `InoTicket`, or `create_at`.
Neither this tree nor `main` contains those APIs, so the test cannot use them, and an OLD-FAIL caused by a missing name would be a compile error rather than the real identity assertion this task requires.

## The failure, as it happens today

On the current tree, `Core::create` returns a session-local virtual inode number, not a durable identity.
`Inner::alloc_virt` mints a number with the top bit `VIRT = 1 << 63` set from Core's own mark files.
`create` queues an `Op::Create` carrying that virtual number, and only the metadata `Tx` picks a real meta number.
Core then records the bridge from the virtual number to the meta number in an in-memory alias table.
The virtual number is a session-local alias: it is not the durable identity, it is not reservation-backed, and it is gone after a reopen.

The test follows one created file from the uncommitted create, through an explicit flush and `sync`, into a fresh `Core::open`, and compares the number the caller held with the number the fresh session resolves.

Observed OLD result, from a real run of the test binary:

```text
assertion `left == right` failed: created file got a session-local virtual number:
created(in-session)=0x8000010000000001 created_meta_before_flush=None
created_meta_after_flush=Some(2) reopen=(ino=0x10000000002, meta=Some(2))
  left: 9223372036854775808
 right: 0
```

Read against the contract:

- `created(in-session)=0x8000010000000001` has the `VIRT` bit set, so the caller holds a virtual alias.
- `created_meta_before_flush=None` confirms meta had no inode behind the caller's number at the moment of create.
- `created_meta_after_flush=Some(2)` shows meta assigned the real number `2` and Core bridged it in the alias table.
- `reopen=(ino=0x10000000002, meta=Some(2))` shows a fresh session resolves the file to a different number than the one the caller held.

The two ends of the path disagree on the file's identity.
That disagreement is the defect this regression locks down.

## Assertion order

The first assertion that fires is the `VIRT` bit check at line 102.
The test still gathers the whole path before asserting, so the panic message carries the full `report` string with all four observations (`created(in-session)`, `created_meta_before_flush`, `created_meta_after_flush`, `reopen`).
The durable-identity-across-reopen clause is therefore both visible in the failure evidence and enforced by the later assertions, which run as soon as the earlier clauses are fixed.

## Tests in the file

`crates/cowfs-core/tests/reserved_inode_identity.rs` has three tests.

1. `a_created_file_keeps_one_durable_identity_across_a_flush_and_reopen` (positive, the load-bearing case).
   Creates a file while it is uncommitted, records the caller's number and the meta inode behind it, flushes and syncs, then reopens and compares the number and the meta identity.
   Asserts the caller's number is not virtual, that meta can name it before the flush, that the number is unchanged across the reopen, that the durable meta identity behind it is unchanged, and that the bytes are preserved.
2. `a_created_number_is_never_the_virtual_alias_shape_or_the_root` (negative control).
   Asserts a created number is not the mount root and does not carry the `VIRT` bit, and that meta has an inode behind it.
3. `a_number_from_a_closed_session_is_stale_after_a_reopen` (negative control, refusal path).
   Asserts a number the fresh session never handed out resolves to `Error::Stale`, not to another file's bytes.
   This property must hold under either identity scheme, and it passes on OLD.

## How the contract is checked with the existing API

The test uses `Core::meta_inode(ino) -> Option<u64>`, the existing public test seam, to read the physical meta number behind an inode.
That reads Core's in-memory alias table and does not open a second concurrent redb handle.
The test also uses `Vfs::flush(&c, ino)`, the explicit trait call, because the inherent `Core::flush(&self)` takes no arguments and would otherwise shadow it.

## Old and new behavior

- OLD (this tree): the first-session number carries the `VIRT` bit, meta does not name it before the commit, and the reopened session resolves the file to the packed meta number instead, so the number the caller held is not the durable identity.
- NEW (the seam this test is written to drive): create draws its number from a real reservation, that number is meta-backed before the flush, and the same number comes back after the reopen with no `VIRT` bit involved.

## Reproduce

From the worktree at branch `fix/core-reserved-inode-consumer-42`, HEAD `89353e17e5085000711dc428e834f9cc41840a1f`:

```sh
CARGO_TARGET_DIR=$PWD/target cargo test -p cowfs-core --test reserved_inode_identity -- --nocapture --test-threads=1
```

Observed result on OLD: `1 passed; 2 failed`, exit code 101.
`rustfmt --edition 2021 --check` on the test file exits 0.

## Provenance

- Test file: `crates/cowfs-core/tests/reserved_inode_identity.rs`, new.
- Test source SHA-256: `e5ded873aa9c1b43c9d87a977aaab05a3a504a6a58118d7cf8c07e2ba124ce40`.
- Worktree: `cowfs45`, branch `fix/core-reserved-inode-consumer-42`, base `89353e17e5085000711dc428e834f9cc41840a1f` (`origin/main` at time of writing).
- No production Core or Meta code, schema, or selected-ID API was changed by this task.
- The run above was local, on the Apple M3 Max Mac, against an unmodified OLD tree.
