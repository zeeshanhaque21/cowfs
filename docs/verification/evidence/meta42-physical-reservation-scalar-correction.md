# PR #142 scalar inode correction

Refs #42, request 4.

The Core consumer retains store/session-bound one-use metadata reservations and physical file identity.
The physical durability and retry fixtures at 71cd5e6 are described in meta42-physical-reservation-durability-retarget.md.
Run 37547782630 failed both platforms before tests ran, during all-target clippy compilation.
The two E0610 errors at durability.rs lines 137 and 188 were my incorrect use of a.ino.0; Core VFS Ino is a u64 alias, unlike Meta's inode wrapper.
Commit 4056e20 changes both expressions to a.ino with the same physical-bit mask and equality assertion.
No assertion, fixture behavior, or production code changed in this correction.
Standalone rustfmt check passes.
Named physical reservation, retry, legacy writer, crash, identity, and conformance runtime results remain required on the new head.
Normal push-triggered CI is pending; no local Cargo build or runner action was performed.
No claim of no-persistence-on-error or whole #42 completion is made.
The PR remains draft and prior evidence documents remain unchanged.
