# NFS #19: MKNOD refusals and PATHCONF values receipt

Branch: test/nfs-mknod-pathconf-19
Commit: 7111d90b4322ef1df866ac6a2f75ea784976db39
Base: bc3ea7d33e3ca55ccb14e0f418a7fd223e3032d8 (main)
File: crates/cowfs-nfs/tests/mknod_pathconf19.rs (new, 59 lines)
Triage: docs/verification/evidence/nfs19-typed-create-pathconf-triage.md

## Assertions

Test `mknod_refuses_fifo_socket_and_devices_without_creating_anything`:

- MKNOD (procedure 11) on the root for FIFO, SOCK, CHR and BLK returns NFS3ERR_NOTSUPP.
- After each refusal, LOOKUP of that name returns NFS3ERR_NOENT.
- A READDIRPLUS listing of the root after the four calls equals the listing taken before them.

Test `pathconf_reports_the_advertised_values`:

- PATHCONF (procedure 20) on the root returns NFS3_OK.
- name_max is 255, no_trunc is true, chown_restricted is true, case_insensitive is false, case_preserving is true.
- linkmax is 65000.

## Limitations

- linkmax 65000 is the nfsserve default (crates/nfsserve/src/vfs.rs:176). cowfs does not enforce it. The test pins the advertised value only and does not enforce it. Known limitation, tracked in #19.
- The MKNOD arguments are encoded by hand (diropargs3, ftype3, sattr3, and specdata3 for CHR and BLK). nfsserve has no typed MKNOD arguments. The handler ignores them and returns NOTSUPP (crates/nfsserve/src/nfs_handlers.rs:134-137).
- chown_restricted, case_insensitive and case_preserving are nfsserve defaults. The test pins them. It does not verify cowfs behaviour behind them.

## Verification

- rustfmt --edition 2021 --check on the new file: exit 0.
- Not run: cargo build and cargo test (local cargo disabled for disk limits). No local runtime, no mount, no pjdfstest.
- CI: pending. Not waited on.
