# PR #142 legacy marker initialization correction

Refs #42, request 4.

Run 37548232902 on 4056e20 passed formatting and compilation, then failed the new legacy ordering unit test on both platforms.
The exact mismatch was Damaged(12345) versus Value(12345) at ino.rs line 288.
Only one legacy mark copy had been created, and the real reader correctly reports damage when the other copy is missing.
This was my fixture initialization error, not a production marker defect.

Commit 691ead3 seeds both alternating copies with 12343 and 12344 before arming the trace and writing 12345.
The existing ordering and exact Value(12345) assertions remain unchanged.
The actual extracted read/write helpers, full fsops implementation, and both new unit-test bodies ran against real filesystem files with a minimal standard-library temporary-directory adapter.
Both corrected helper cases passed, 2/2 with exit 0.
Removing only the two seeding writes reproduced Damaged(12345) versus Value(12345), exit 101.
This is helper-level filesystem execution, not full Core physical reservation acceptance.
Standalone rustfmt passes; full exact-head CI and the physical durability/refusal/retry cases remain pending.

The small extracted binaries, sources, and fixtures are preserved under READY3 bench/out/legacy-mark-691ead3, with moved binary checksums verified.
No production code, runtime assertion, public seam, runner, workflow, shared artifact, store, mount, or lease was changed or discarded.
Historical receipts remain unchanged and the PR remains draft.
